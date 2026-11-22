use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;
use std::{io, thread};

use super::entries::{read_entries, remote_entries, sort_entries};
use super::{Entry, LOAD_WAIT, Panel, SortKey};
use crate::vfs::Remote;
use crate::{Result, io_err};

/// What to do once a listing read in the background arrives.
#[derive(Debug)]
pub(super) enum Then {
    /// Show the new directory: set history position `pos` or record a `visit`, then focus a name.
    Show {
        pos: Option<usize>,
        visit: bool,
        focus: Option<OsString>,
    },
    /// Re-read of the shown directory: keep cursor and selection.
    Reload,
    /// Re-stat of a virtual list: like `Reload`, vanished entries drop out.
    Restat,
}

/// A directory listing read in a worker; the panel shows the old list until it arrives.
#[derive(Debug)]
pub(super) struct Pending {
    pub(super) path: PathBuf,
    pub(super) sort: (SortKey, bool),
    pub(super) then: Then,
    pub(super) rx: Receiver<Result<Vec<Entry>>>,
    pub(super) cancel: Arc<AtomicBool>,
    pub(super) remote: Option<Arc<dyn Remote>>,
}

impl Panel {
    /// Read `path` in a worker; fast directories are applied before returning, slow ones by [`Panel::poll`].
    /// A failure leaves the panel unchanged.
    pub(super) fn load(&mut self, path: PathBuf, then: Then) -> Result<()> {
        self.load_on(self.remote.clone(), path, then)
    }

    /// Like [`Panel::load`] on `remote` (`None` = local disk).
    pub(super) fn load_on(
        &mut self,
        remote: Option<Arc<dyn Remote>>,
        path: PathBuf,
        then: Then,
    ) -> Result<()> {
        let (dir, sort) = (path.clone(), self.sort);
        let r = remote.clone();
        self.start(remote, path, then, move |c| match r {
            Some(r) => remote_entries(&*r, &dir, sort),
            None => read_entries(&dir, sort, c),
        })
    }

    /// Run `job` in a worker; fast results are applied before returning, slow ones by [`Panel::poll`].
    pub(super) fn start(
        &mut self,
        remote: Option<Arc<dyn Remote>>,
        path: PathBuf,
        then: Then,
        job: impl FnOnce(&AtomicBool) -> Result<Vec<Entry>> + Send + 'static,
    ) -> Result<()> {
        self.cancel_load();
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let (c, sort) = (cancel.clone(), self.sort);
        // a hung network mount or server blocks this thread, not the UI; cancel only drops its result
        thread::spawn(move || drop(tx.send(job(&c))));
        self.pending = Some(Pending {
            path,
            sort,
            then,
            rx,
            cancel,
            remote,
        });
        self.wait(LOAD_WAIT).unwrap_or(Ok(()))
    }

    /// Apply a finished background listing; `None` while it is still read or nothing is pending.
    pub fn poll(&mut self) -> Option<Result<()>> {
        self.wait(Duration::ZERO)
    }

    pub(super) fn wait(&mut self, timeout: Duration) -> Option<Result<()>> {
        let pending = self.pending.as_ref()?;
        let res = match pending.rx.recv_timeout(timeout) {
            Err(RecvTimeoutError::Timeout) => return None,
            Ok(res) => res,
            Err(RecvTimeoutError::Disconnected) => {
                Err(io_err(&pending.path)(io::Error::other("listing failed")))
            }
        };
        let p = self.pending.take()?;
        Some(res.map(|mut entries| {
            if p.sort != self.sort {
                let start = usize::from(entries.first().is_some_and(Entry::is_parent));
                sort_entries(&mut entries[start..], self.sort);
            }
            match p.then {
                Then::Reload | Then::Restat => self.reloaded(entries),
                Then::Show { pos, visit, focus } => {
                    self.show(p.path, entries);
                    self.remote = p.remote;
                    if let Some(i) = pos {
                        self.pos = i;
                    }
                    // remote directories are not in the (local) history
                    if visit && self.remote.is_none() {
                        self.visit(&self.path.clone());
                        self.visited = Some(self.path.clone());
                    }
                    if let Some(name) = focus {
                        self.focus(&name);
                    }
                }
            }
        }))
    }

    /// Directory whose listing is still being read.
    pub fn loading(&self) -> Option<&Path> {
        self.pending.as_ref().map(|p| p.path.as_path())
    }

    /// Stop waiting for a pending listing; true if there was one.
    pub fn cancel_load(&mut self) -> bool {
        self.pending
            .take()
            .inspect(|p| p.cancel.store(true, Ordering::Relaxed))
            .is_some()
    }

    pub(super) fn show(&mut self, path: PathBuf, entries: Vec<Entry>) {
        self.entries = entries;
        self.dir_sizes.clear();
        self.sel_sum.set(None);
        self.all = None;
        self.filter.clear();
        self.virt = false;
        self.hits.clear();
        self.archive = None;
        self.path = path;
        self.cursor = 0;
        self.selected.clear();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn slow_listing_keeps_old_list_until_it_arrives() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("f"), "x").unwrap();
        let mut p = Panel::open(tmp.path()).unwrap();
        // stand-in for a worker on a slow mount
        let (tx, rx) = mpsc::channel();
        let then = Then::Show {
            pos: None,
            visit: true,
            focus: Some("f".into()),
        };
        let cancel = Arc::new(AtomicBool::new(false));
        p.pending = Some(Pending {
            path: sub.clone(),
            sort: p.sort,
            then,
            rx,
            cancel,
            remote: None,
        });
        assert!(p.poll().is_none());
        assert_eq!(p.path(), tmp.path()); // still the old directory
        assert_eq!(p.loading(), Some(sub.as_path()));
        let never = AtomicBool::new(false);
        tx.send(read_entries(&sub, p.sort, &never)).unwrap();
        assert!(p.poll().unwrap().is_ok());
        assert_eq!((p.path(), p.loading()), (sub.as_path(), None));
        assert_eq!(p.current().unwrap().name, "f");
        p.history_step(-1).unwrap();
        assert_eq!(p.path(), tmp.path()); // visit was recorded
    }

    #[test]
    fn cancel_drops_pending_listing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = Panel::open(tmp.path()).unwrap();
        let (_tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let then = Then::Reload;
        let path = tmp.path().join("x");
        p.pending = Some(Pending {
            path,
            sort: p.sort,
            then,
            rx,
            cancel: cancel.clone(),
            remote: None,
        });
        assert!(p.cancel_load());
        assert!(cancel.load(Ordering::Relaxed) && p.loading().is_none() && !p.cancel_load());
        fs::write(tmp.path().join("a"), "").unwrap();
        assert!(
            read_entries(tmp.path(), p.sort, &cancel)
                .unwrap()
                .is_empty()
        );
    }
}
