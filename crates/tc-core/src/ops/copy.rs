//! Local copy of files and trees: OS copy (clone on macOS, `CopyFileExW` on Windows) where it
//! applies, several files of a tree at once.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};

#[cfg(not(target_os = "linux"))]
use super::CHUNK;
use super::{Progress, platform};
use crate::{Error, Result, io_err};

/// Files copied at once inside one tree: hides per-file latency (open/close, antivirus scans on
/// Windows, network mounts), which dominates for many small files.
// shortcut: fixed count, may seek more on spinning disks; make it a setting if users report that
const WORKERS: usize = 4;

/// Source, target, source metadata.
type Job = (PathBuf, PathBuf, fs::Metadata);

/// Copy `src` (file, symlink or directory tree) to the new path `dst`. Leaves partial targets
/// on error; callers remove them. Sockets, pipes and devices cannot be copied: `skip_special`
/// skips them, otherwise they fail with [`Error::Special`] (a move must not delete them).
pub(super) fn copy_tree(src: &Path, dst: &Path, p: &Progress, skip_special: bool) -> Result<()> {
    let meta = fs::symlink_metadata(src).map_err(io_err(src))?;
    if !meta.is_dir() {
        return copy_leaf(src, dst, &meta, p, skip_special).map(drop);
    }
    let (tx, rx) = mpsc::sync_channel::<Job>(256);
    let rx = Mutex::new(rx);
    let failed = Mutex::new(None);
    let stop = AtomicBool::new(false);
    let mut dirs = Vec::new();
    std::thread::scope(|s| {
        for _ in 0..WORKERS {
            s.spawn(|| worker(&rx, &failed, &stop, p));
        }
        let mut w = Walk {
            tx,
            dirs: &mut dirs,
            stop: &stop,
            p,
            skip_special,
        };
        if let Err(e) = w.dir(src, dst, &meta) {
            fail(&failed, &stop, e);
        }
        // dropping `w` closes the channel: workers finish the queue and exit
    });
    if let Some(e) = failed
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
    {
        return Err(e);
    }
    // deepest first and after the contents, so a read-only directory does not block them
    for (d, perm) in dirs.into_iter().rev() {
        fs::set_permissions(&d, perm).map_err(io_err(&d))?;
    }
    Ok(())
}

/// Keep the first error, stop the walk and the queued copies.
fn fail(failed: &Mutex<Option<Error>>, stop: &AtomicBool, e: Error) {
    stop.store(true, Ordering::Relaxed);
    if let Ok(mut f) = failed.lock() {
        f.get_or_insert(e);
    }
}

fn worker(
    rx: &Mutex<mpsc::Receiver<Job>>,
    failed: &Mutex<Option<Error>>,
    stop: &AtomicBool,
    p: &Progress,
) {
    loop {
        let Ok(Ok((s, d, m))) = rx.lock().map(|r| r.recv()) else {
            return;
        };
        // drain without copying, so the walker never blocks on a full queue
        if stop.load(Ordering::Relaxed) {
            continue;
        }
        if let Err(e) = p.retry(&s, || copy_file(&s, &d, &m, p, false)) {
            fail(failed, stop, e);
        }
    }
}

/// Creates directories and symlinks itself, queues files for the workers.
struct Walk<'a> {
    tx: mpsc::SyncSender<Job>,
    /// Created directories with the permissions to set once all files are in.
    dirs: &'a mut Vec<(PathBuf, fs::Permissions)>,
    stop: &'a AtomicBool,
    p: &'a Progress,
    skip_special: bool,
}

impl Walk<'_> {
    fn dir(&mut self, src: &Path, dst: &Path, meta: &fs::Metadata) -> Result<()> {
        fs::create_dir(dst).map_err(io_err(dst))?;
        self.dirs.push((dst.to_owned(), meta.permissions()));
        for item in fs::read_dir(src).map_err(io_err(src))? {
            if self.p.cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            if self.stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let item = item.map_err(io_err(src))?;
            let (s, d) = (item.path(), dst.join(item.file_name()));
            // from the listing on Windows, saves an open per file; never follows symlinks
            let m = item.metadata().map_err(io_err(&s))?;
            if m.is_dir() {
                self.dir(&s, &d, &m)?;
            } else if m.is_symlink() || is_special(&m) {
                copy_leaf(&s, &d, &m, self.p, self.skip_special)?;
            } else if self.tx.send((s, d, m)).is_err() {
                return Ok(());
            }
        }
        self.p.dir_done();
        Ok(())
    }
}

/// File or symlink; failures go to [`Progress::retry`]. False when the user skipped it.
fn copy_leaf(
    src: &Path,
    dst: &Path,
    meta: &fs::Metadata,
    p: &Progress,
    skip_special: bool,
) -> Result<bool> {
    if is_special(meta) {
        if !skip_special {
            return Err(Error::Special(src.to_owned()));
        }
        // shortcut: skipped silently, report a skip count if users miss them
        p.files.fetch_add(1, Ordering::Relaxed);
        Ok(true)
    } else if meta.is_symlink() {
        p.retry(src, || {
            let link = fs::read_link(src).map_err(io_err(src))?;
            platform::symlink(&link, dst, src).map_err(io_err(dst))
        })
    } else {
        p.retry(src, || copy_file(src, dst, meta, p, false))
    }
}

/// Socket, FIFO or device: opening one fails or blocks forever, its content is not a file.
pub(crate) fn is_special(meta: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        let t = meta.file_type();
        t.is_socket() || t.is_fifo() || t.is_block_device() || t.is_char_device()
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

/// Copy keeping the modification time and permissions; progress and cancel work inside a huge
/// file. Never replaces an existing `dst`. `sync`: flush to disk before it replaces a file.
/// Removes its partial `dst` on error (a retry or skip leaves no half file).
pub(super) fn copy_file(
    src: &Path,
    dst: &Path,
    meta: &fs::Metadata,
    p: &Progress,
    sync: bool,
) -> Result<()> {
    p.current_is(src);
    if p.cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    // only a file this copy created is removed, never one that was there
    let existed = fs::symlink_metadata(dst).is_ok();
    let r = match os_copy(src, dst, meta, p, sync) {
        Some(r) => r,
        None => stream(src, dst, meta, p, sync),
    };
    // shortcut: bytes of a failed try stay counted, a retry overshoots `done`; fine for a progress bar
    if r.is_err() && !existed {
        let _ = fs::remove_file(dst);
    }
    r?;
    p.files.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// Chunked copy through [`platform::copy_chunk`].
fn stream(src: &Path, dst: &Path, meta: &fs::Metadata, p: &Progress, sync: bool) -> Result<()> {
    let from = fs::File::open(src).map_err(io_err(src))?;
    // create_new: a file that appeared since the existence check is never truncated
    let mut to = fs::File::create_new(dst).map_err(io_err(dst))?;
    let mut buf = Vec::new();
    loop {
        if p.cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let n = platform::copy_chunk(&from, &mut to, &mut buf).map_err(io_err(dst))?;
        if n == 0 {
            break;
        }
        p.done.fetch_add(n, Ordering::Relaxed);
    }
    if let Ok(t) = meta.modified() {
        let _ = to.set_modified(t);
    }
    if sync {
        to.sync_all().map_err(io_err(dst))?;
    }
    fs::set_permissions(dst, meta.permissions()).map_err(io_err(dst))
}

/// Read and write up to [`CHUNK`] bytes through `buf`; 0 at end of file.
#[cfg(not(target_os = "linux"))]
pub(super) fn copy_chunk_buffered(
    from: &fs::File,
    to: &mut fs::File,
    buf: &mut Vec<u8>,
) -> std::io::Result<u64> {
    use std::io::{Read, Write};
    if buf.is_empty() {
        // zeroed alloc: untouched pages stay unmapped, cheap for many small files
        *buf = vec![0; CHUNK];
    }
    let n = (&*from).read(buf)?;
    to.write_all(&buf[..n])?;
    Ok(n as u64)
}

/// No OS whole-file copy here: Linux' [`platform::copy_chunk`] already runs in the kernel.
#[cfg(not(any(target_os = "macos", windows)))]
fn os_copy(_: &Path, _: &Path, _: &fs::Metadata, _: &Progress, _: bool) -> Option<Result<()>> {
    None
}

/// APFS clone: instant, shares blocks until written, keeps mode and times. `None` (nothing
/// created) on volumes without clones.
#[cfg(target_os = "macos")]
fn os_copy(
    src: &Path,
    dst: &Path,
    meta: &fs::Metadata,
    p: &Progress,
    sync: bool,
) -> Option<Result<()>> {
    use rustix::fs::{CWD, CloneFlags, fclonefileat};
    use rustix::io::Errno;
    let from = match fs::File::open(src) {
        Ok(f) => f,
        Err(e) => return Some(Err(io_err(src)(e))),
    };
    // clonefile is atomic: on failure nothing was created. EEXIST: never replace a target;
    // any other error (no clones on this volume, other device, ...) takes the stream copy
    match fclonefileat(&from, CWD, dst, CloneFlags::empty()) {
        Ok(()) => {}
        Err(e) if e == Errno::EXIST => return Some(Err(io_err(dst)(e.into()))),
        Err(_) => return None,
    }
    if sync && let Err(e) = fs::File::open(dst).and_then(|f| f.sync_all()) {
        return Some(Err(io_err(dst)(e)));
    }
    p.done.fetch_add(meta.len(), Ordering::Relaxed);
    Some(Ok(()))
}

/// `CopyFileExW`: one call per file, keeps times and attributes, server-side copy on SMB.
/// `None` (nothing created) when the stream copy has to do it.
#[cfg(windows)]
fn os_copy(
    src: &Path,
    dst: &Path,
    _: &fs::Metadata,
    p: &Progress,
    sync: bool,
) -> Option<Result<()>> {
    use std::cell::Cell;
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{ERROR_REQUEST_ABORTED, HANDLE};
    use windows_sys::Win32::Storage::FileSystem::{
        COPY_FILE_FAIL_IF_EXISTS, CopyFileExW, PROGRESS_CANCEL, PROGRESS_CONTINUE,
    };

    struct Ctx<'a> {
        p: &'a Progress,
        last: Cell<u64>,
    }

    unsafe extern "system" fn progress(
        _total: i64,
        done: i64,
        _stream_total: i64,
        _stream_done: i64,
        _stream: u32,
        _reason: u32,
        _src: HANDLE,
        _dst: HANDLE,
        data: *const c_void,
    ) -> u32 {
        // SAFETY: `data` is the `Ctx` passed to CopyFileExW below, alive during the call
        let ctx = unsafe { &*data.cast::<Ctx>() };
        let done = u64::try_from(done).unwrap_or(0);
        let p = ctx.p;
        p.done
            .fetch_add(done.saturating_sub(ctx.last.get()), Ordering::Relaxed);
        ctx.last.set(done);
        if p.cancel.load(Ordering::Relaxed) {
            PROGRESS_CANCEL
        } else {
            PROGRESS_CONTINUE
        }
    }

    let wide = |p: &Path| -> Vec<u16> { p.as_os_str().encode_wide().chain([0]).collect() };
    let (s, d) = (wide(src), wide(dst));
    // shortcut: plain paths stop at MAX_PATH (260); longer ones take the stream copy, which
    // std prefixes with \\?\. Prefix here too if deep trees turn out slow.
    // sync: the copied read-only flag would block opening the target for the flush
    if sync || s.len() > 260 || d.len() > 260 {
        return None;
    }
    let ctx = Ctx {
        p,
        last: Cell::new(0),
    };
    // SAFETY: both names are NUL-terminated and `ctx` outlives the synchronous call
    let ok = unsafe {
        CopyFileExW(
            s.as_ptr(),
            d.as_ptr(),
            Some(progress),
            (&raw const ctx).cast(),
            std::ptr::null_mut(),
            COPY_FILE_FAIL_IF_EXISTS,
        )
    };
    if ok != 0 {
        return Some(Ok(()));
    }
    let e = std::io::Error::last_os_error();
    Some(Err(
        if e.raw_os_error() == Some(ERROR_REQUEST_ABORTED as i32) {
            Error::Cancelled
        } else {
            io_err(dst)(e)
        },
    ))
}
