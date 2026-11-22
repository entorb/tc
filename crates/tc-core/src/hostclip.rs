//! Files on the host's clipboard (copied in Explorer, Finder or a file manager). Read with the
//! system's own tools, so no clipboard crate is needed for files.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Paths of the files on the host clipboard; empty when there are none or no tool can read them.
pub fn read_files() -> Vec<PathBuf> {
    platform_files().unwrap_or_default()
}

/// Puts `paths` on the host clipboard as files, so Explorer, Finder or a file manager can paste
/// them. False when no tool can do it. Takes a while (starts a process): call off the UI thread.
pub fn write_files(paths: &[PathBuf]) -> bool {
    platform_write(paths).is_some()
}

#[cfg(any(test, all(unix, not(target_os = "macos"))))]
/// `file://` URIs of a `text/uri-list` (one per line, `#` comments skipped), percent-decoded.
pub fn parse_uri_list(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("file://"))
        // `file://localhost/p` names the same local file as `file:///p`
        .map(|p| {
            p.strip_prefix("localhost")
                .filter(|r| r.starts_with('/'))
                .unwrap_or(p)
        })
        .map(|p| path_from_bytes(percent_decode(p)))
        .collect()
}

/// One path per non-empty line.
#[cfg(any(windows, target_os = "macos"))]
fn parse_lines(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect()
}

#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = b
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (b[i], hex) {
            (b'%', Some(v)) => {
                out.push(v);
                i += 3;
            }
            _ => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    out
}

#[cfg(any(test, all(unix, not(target_os = "macos"))))]
#[cfg(unix)]
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

#[cfg(any(test, all(unix, not(target_os = "macos"))))]
#[cfg(not(unix))]
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(any(test, all(unix, not(target_os = "macos"))))]
/// `file://` URI of an absolute path, percent-encoded (inverse of [`parse_uri_list`]).
fn file_uri(path: &std::path::Path) -> String {
    let mut uri = String::from("file://");
    for b in path_bytes(path) {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                uri.push(char::from(b))
            }
            _ => uri.push_str(&format!("%{b:02X}")),
        }
    }
    uri
}

#[cfg(any(test, all(unix, not(target_os = "macos"))))]
#[cfg(unix)]
fn path_bytes(path: &std::path::Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(any(test, all(unix, not(target_os = "macos"))))]
#[cfg(not(unix))]
fn path_bytes(path: &std::path::Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
}

#[cfg(any(windows, all(unix, not(target_os = "macos"))))]
/// Stdin of `prog args` set to `data`, stdout and stderr dropped (tools that fork keep the
/// pipes otherwise); Some when it succeeds.
fn feed(prog: &str, args: &[&str], data: &str) -> Option<()> {
    use std::io::Write;
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(data.as_bytes()).ok()?;
    child.wait().ok()?.success().then_some(())
}

/// Stdout of `prog args` when it runs and succeeds.
fn output(prog: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(prog)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Linux: `wl-copy` (Wayland) or `xclip` (X11) with the `text/uri-list` target.
#[cfg(all(unix, not(target_os = "macos")))]
fn platform_write(paths: &[PathBuf]) -> Option<()> {
    let uris: String = paths.iter().map(|p| file_uri(p) + "\r\n").collect();
    feed("wl-copy", &["--type", "text/uri-list"], &uris).or_else(|| {
        feed(
            "xclip",
            &["-selection", "clipboard", "-target", "text/uri-list", "-i"],
            &uris,
        )
    })
}

/// Linux: Wayland's `wl-paste`, else X11's `xclip`, asking for the `text/uri-list` target.
#[cfg(all(unix, not(target_os = "macos")))]
fn platform_files() -> Option<Vec<PathBuf>> {
    let wayland = output("wl-paste", &["--no-newline", "--type", "text/uri-list"]);
    let x11 = || {
        output(
            "xclip",
            &[
                "-selection",
                "clipboard",
                "-target",
                "text/uri-list",
                "-out",
            ],
        )
    };
    wayland.or_else(x11).map(|t| parse_uri_list(&t))
}

/// macOS: the paths as file URLs on the pasteboard, through JavaScript for Automation.
#[cfg(target_os = "macos")]
fn platform_write(paths: &[PathBuf]) -> Option<()> {
    const JXA: &str = "function run(argv) { ObjC.import('AppKit'); \
        const pb = $.NSPasteboard.generalPasteboard; pb.clearContents; \
        const urls = $.NSMutableArray.alloc.init; \
        argv.forEach(p => urls.addObject($.NSURL.fileURLWithPath(p))); \
        return pb.writeObjects(urls); }";
    let mut args = vec!["-l", "JavaScript", "-e", JXA];
    let owned: Vec<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    args.extend(owned.iter().map(String::as_str));
    output("osascript", &args)
        .filter(|t| t.trim() == "true")
        .map(|_| ())
}

/// macOS: file URLs of the pasteboard through JavaScript for Automation (`osascript`).
#[cfg(target_os = "macos")]
fn platform_files() -> Option<Vec<PathBuf>> {
    const JXA: &str = "ObjC.import('AppKit'); \
        const urls = $.NSPasteboard.generalPasteboard.readObjectsForClassesOptions([$.NSURL.class], $()); \
        const out = []; \
        for (let i = 0; i < urls.count; i++) { out.push(ObjC.unwrap(urls.objectAtIndex(i).path)); } \
        out.join('\\n');";
    output("osascript", &["-l", "JavaScript", "-e", JXA]).map(|t| parse_lines(&t))
}

/// Windows: `SetFileDropList`, the list Explorer puts on copy (paths on stdin, one per line).
#[cfg(windows)]
fn platform_write(paths: &[PathBuf]) -> Option<()> {
    // UTF-8 both ways: the console default (OEM code page) mangles non-ASCII names
    const PS: &str = "[Console]::InputEncoding = [Text.Encoding]::UTF8; \
        Add-Type -AssemblyName System.Windows.Forms; \
        $c = New-Object System.Collections.Specialized.StringCollection; \
        [Console]::In.ReadToEnd().Split(\"`n\") | Where-Object { $_ -ne '' } | \
        ForEach-Object { [void]$c.Add($_.TrimEnd(\"`r\")) }; \
        [System.Windows.Forms.Clipboard]::SetFileDropList($c)";
    let data: String = paths.iter().map(|p| format!("{}\n", p.display())).collect();
    feed("powershell", &["-NoProfile", "-STA", "-Command", PS], &data)
}

/// Windows: `Get-Clipboard -Format FileDropList` (what Explorer puts on copy).
#[cfg(windows)]
fn platform_files() -> Option<Vec<PathBuf>> {
    const PS: &str = "[Console]::OutputEncoding = [Text.Encoding]::UTF8; \
        Get-Clipboard -Format FileDropList | ForEach-Object { $_.FullName }";
    output("powershell", &["-NoProfile", "-STA", "-Command", PS]).map(|t| parse_lines(&t))
}

#[cfg(test)]
mod tests {
    #[cfg(any(windows, target_os = "macos"))]
    use super::parse_lines;
    use super::{PathBuf, file_uri, parse_uri_list, percent_decode};

    #[test]
    fn uri_list_decodes_and_skips_other_lines() {
        let text =
            "# copied\r\nfile:///home/t/My%20docs/a%C3%A4.txt\r\nhttps://x/y\r\nfile:///tmp/b\r\n";
        assert_eq!(
            parse_uri_list(text),
            [
                PathBuf::from("/home/t/My docs/aä.txt"),
                PathBuf::from("/tmp/b")
            ]
        );
    }

    #[test]
    fn file_uri_round_trips_through_parse() {
        let p = PathBuf::from("/home/t/My docs/aä #1%.txt");
        assert_eq!(parse_uri_list(&file_uri(&p)), [p]);
    }

    #[test]
    fn localhost_uri_is_local_path() {
        assert_eq!(
            parse_uri_list("file://localhost/tmp/a"),
            [PathBuf::from("/tmp/a")]
        );
    }

    #[test]
    fn percent_decode_keeps_broken_escapes() {
        assert_eq!(percent_decode("a%2"), b"a%2");
        assert_eq!(percent_decode("%zz%41"), b"%zzA");
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn plain_lines_are_paths() {
        assert_eq!(
            parse_lines("C:\\Users\\t\\a.txt\r\n\r\nD:\\b\r\n"),
            [PathBuf::from("C:\\Users\\t\\a.txt"), PathBuf::from("D:\\b")]
        );
    }
}
