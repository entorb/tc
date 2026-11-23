//! Remote sessions: connect (Ctrl+N list, host key and password prompts), fetch for the editor, disconnect.

use super::{App, Done, Filter, Mode, VIEW_CAP, edit_key, list_nav, list_rows};
use crate::{KeyCode, KeyEvent};
use std::mem;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use tc_core::cmdline::LineEdit;
use tc_core::vfs::{self, Remote, Side};
use tc_core::{Error, hotlist};
use tc_remote::Prompt;

impl App {
    /// Panels and tabs on a remote go back to their local directory; remembered remote
    /// clipboard, undo and sync state is dropped, so each session closes with its last user.
    pub(super) fn disconnect_all(&mut self) {
        let remote = |s: &Side| matches!(s, Side::Remote(_));
        let panels = self.panels.iter_mut().chain(self.tabs.iter_mut().flatten());
        let errors: Vec<String> = panels
            .filter_map(|p| p.disconnect().err())
            .map(|e| e.to_string())
            .collect();
        self.file_clip.take_if(|c| remote(&c.side));
        self.rename_undo.take_if(|b| remote(&b.side));
        self.sync_back.take_if(|s| s.sides.iter().any(remote));
        self.sync_again.take_if(|s| s.1.iter().any(remote));
        self.found.take_if(|f| f.remote);
        if !errors.is_empty() {
            self.mode = Mode::Message(errors.join("\n"));
        }
    }
}

pub(super) enum CMsg {
    Ask(Prompt),
    Done(tc_core::Result<(Arc<dyn Remote>, String)>),
}

pub struct Connecting {
    pub url: String,
    /// Open question and the answer typed so far (a password, shown masked).
    pub ask: Option<(Prompt, LineEdit)>,
    pub(super) rx: Receiver<CMsg>,
    pub(super) reply: mpsc::Sender<Option<String>>,
}

impl App {
    pub(super) fn connections_save(&mut self) {
        if let Some(file) = &self.connections_file
            && let Err(e) = hotlist::save(file, &self.connections)
        {
            self.mode = Mode::Message(e.to_string());
        }
    }

    pub(super) fn connections_key(&mut self, key: KeyEvent, sel: usize) {
        let mut sel = sel.min(self.connections.len().saturating_sub(1));
        if list_nav(&mut self.list_filter, &self.connections, &mut sel, key) {
            self.mode = Mode::Connections(sel);
            return;
        }
        let shown = list_rows(&self.connections, &self.list_filter).contains(&sel);
        let url = self
            .connections
            .get(sel)
            .filter(|_| shown)
            .map(|u| u.to_string_lossy().into_owned());
        match (key.code, url) {
            (KeyCode::Esc | KeyCode::F(10) | KeyCode::Char('q'), _) => return,
            (KeyCode::Enter, Some(url)) => return self.connect(url),
            (KeyCode::Insert | KeyCode::Char('a'), _) => {
                self.list_filter = Filter::default();
                self.mode = Mode::ConnEdit(None, LineEdit::from("sftp://".to_owned()));
                return;
            }
            (KeyCode::F(4) | KeyCode::Char('e'), Some(url)) => {
                self.list_filter = Filter::default();
                self.mode = Mode::ConnEdit(Some(sel), LineEdit::from(url));
                return;
            }
            (KeyCode::Delete | KeyCode::Backspace, Some(_)) => {
                self.connections.remove(sel);
                self.connections_save();
                sel = list_rows(&self.connections, &self.list_filter)
                    .first()
                    .copied()
                    .unwrap_or(0);
                if matches!(self.mode, Mode::Message(_)) {
                    return;
                }
            }
            _ => {}
        }
        self.mode = Mode::Connections(sel);
    }

    /// Store an added or edited URL (no passwords: they are asked on connect).
    pub(super) fn conn_save(&mut self, row: Option<usize>, text: &str) {
        let text = text.trim();
        if let Err(e) = tc_remote::Url::parse(text) {
            self.mode = Mode::Message(e.to_string());
            return;
        }
        let url = PathBuf::from(text);
        let sel = match row.filter(|&i| i < self.connections.len()) {
            Some(i) => {
                self.connections[i] = url;
                i
            }
            None => {
                self.connections.push(url);
                self.connections.len() - 1
            }
        };
        self.mode = Mode::Connections(sel);
        self.connections_save();
    }

    /// Connect in a worker; questions (host key, password) come back through `tick`.
    pub(super) fn connect(&mut self, url: String) {
        let (tx, rx) = mpsc::channel();
        let (reply, answers) = mpsc::channel::<Option<String>>();
        let u = url.clone();
        thread::spawn(move || {
            let mut ask = |p: Prompt| {
                tx.send(CMsg::Ask(p)).ok()?;
                answers.recv().ok().flatten()
            };
            let result = tc_remote::connect(&u, &mut ask);
            let _ = tx.send(CMsg::Done(result));
        });
        self.mode = Mode::Connecting(Connecting {
            url,
            ask: None,
            rx,
            reply,
        });
    }

    /// Host key: `y` trusts, `n`/Esc cancels; password: typed, Enter sends. Esc while waiting
    /// abandons the attempt (the worker finishes on its own).
    pub(super) fn connecting_key(&mut self, mut c: Connecting, key: KeyEvent) {
        let answer = match (&mut c.ask, key.code) {
            (None, KeyCode::Esc) => return,
            (Some(_), KeyCode::Esc) | (Some((Prompt::HostKey(_), _)), KeyCode::Char('n')) => {
                Some(None)
            }
            (Some((Prompt::HostKey(_), _)), KeyCode::Char('y')) => Some(Some("y".to_owned())),
            (Some((Prompt::Password(_), l)), KeyCode::Enter) => Some(Some(mem::take(&mut l.text))),
            (Some((Prompt::Password(_), l)), _) => {
                edit_key(l, key);
                None
            }
            _ => None,
        };
        if let Some(a) = answer {
            c.ask = None;
            let _ = c.reply.send(a);
        }
        self.mode = Mode::Connecting(c);
    }

    pub(super) fn connect_tick(&mut self) {
        let Mode::Connecting(c) = &mut self.mode else {
            return;
        };
        match c.rx.try_recv() {
            Ok(CMsg::Ask(p)) => c.ask = Some((p, LineEdit::default())),
            Err(TryRecvError::Empty) => {}
            Ok(CMsg::Done(Ok((remote, dir)))) => {
                self.mode = Mode::Normal;
                if let Err(e) = self.panels[self.active].open_remote(remote, dir.into()) {
                    self.mode = Mode::Message(e.to_string());
                }
            }
            Ok(CMsg::Done(Err(Error::Cancelled))) => self.mode = Mode::Normal,
            Ok(CMsg::Done(Err(e))) => self.mode = Mode::Message(format!("{}: {e}", c.url)),
            Err(TryRecvError::Disconnected) => {
                self.mode = Mode::Message("connect worker failed".into())
            }
        }
    }

    /// F3/F4 on a remote panel: download the file to a temp copy (worker), then view or edit it.
    pub(super) fn fetch_remote(&mut self, edit: bool) {
        let panel = &self.panels[self.active];
        let (Some(r), Some(e)) = (panel.remote().cloned(), panel.current()) else {
            return;
        };
        if e.is_dir {
            return;
        }
        let (r, path, size) = (r, panel.path().join(&e.name), e.size);
        self.fetch(r, path, size, edit);
    }

    pub(super) fn fetch(&mut self, r: Arc<dyn Remote>, path: PathBuf, size: u64, edit: bool) {
        if size > VIEW_CAP {
            self.mode = Mode::Message("File over 1 GiB: copy it with F5 instead.".into());
            return;
        }
        let title = PathBuf::from(format!("{}{}", r.url(), vfs::rpath(&path)));
        let total = size;
        self.spawn(
            "Downloading",
            title,
            total,
            false,
            move |progress, cancel| {
                let res = vfs::download_temp(&r, &path, VIEW_CAP, progress, cancel);
                if edit {
                    Done::Fetched(r, path, res)
                } else {
                    Done::Extracted(res)
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {

    use super::super::Mode;

    use crate::{KeyCode, KeyModifiers};

    use std::path::PathBuf;
    use std::sync::Arc;

    use super::super::tests::{key, settle, two_panels, type_keys, with};

    #[test]
    fn disconnect_all_closes_every_remote_hold() {
        let left = tempfile::tempdir().unwrap();
        let mem = tc_core::vfs::mem::Mem::new();
        let mut app = two_panels(left.path(), left.path());
        app.panels[1].open_remote(mem.clone(), "/".into()).unwrap();
        settle(&mut app);
        app.active = 1;
        with(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL); // tab on the same session
        settle(&mut app);
        assert!(Arc::strong_count(&mem) > 2);
        with(&mut app, KeyCode::Null, KeyModifiers::SHIFT);
        settle(&mut app);
        assert_eq!(Arc::strong_count(&mem), 1);
        assert_eq!(app.panels[1].path(), left.path());
        assert!(app.panels[1].remote().is_none());
    }

    #[test]
    fn connection_list_add_edit_delete() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        with(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
        assert!(matches!(app.mode, Mode::Connections(0)));
        key(&mut app, KeyCode::Insert);
        type_keys(&mut app, "me@h");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.connections, [PathBuf::from("sftp://me@h")]);
        key(&mut app, KeyCode::F(4));
        type_keys(&mut app, "ost");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.connections, [PathBuf::from("sftp://me@host")]);
        key(&mut app, KeyCode::Insert);
        for _ in 0..7 {
            key(&mut app, KeyCode::Backspace);
        }
        type_keys(&mut app, "nonsense");
        key(&mut app, KeyCode::Enter);
        assert!(matches!(app.mode, Mode::Message(_)));
        assert_eq!(app.connections.len(), 1);
        key(&mut app, KeyCode::Esc);
        with(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Delete);
        assert!(app.connections.is_empty());
        key(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal));
    }
}
