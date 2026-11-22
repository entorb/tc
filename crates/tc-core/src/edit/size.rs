use std::fs;

use super::{Error, Result, io_err};
use std::path::Path;

/// Above this the user is asked to use the viewer instead.
const LARGE: u64 = 1 << 30;

#[derive(Debug, PartialEq, Eq)]
pub enum SizeCheck {
    Ok,
    Large(u64),
}

pub fn human(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "K", "M", "G"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        return format!("{n} B");
    }
    format!("{v:.1} {}", UNITS[i])
}

/// Size checks from metadata only, nothing is read.
pub fn check_size(path: &Path) -> Result<SizeCheck> {
    let free = sysinfo::System::new().map_or(0, |mut s| {
        s.refresh_memory();
        s.available_memory()
    });
    check_size_with(path, free)
}

pub(crate) fn check_size_with(path: &Path, free: u64) -> Result<SizeCheck> {
    let len = fs::metadata(path).map_err(io_err(path))?.len();
    // rope overhead and UTF-16 -> UTF-8 growth; free == 0 means the OS reported nothing
    let need = len.saturating_mul(2);
    if free != 0 && need > free {
        return Err(Error::OutOfMemory { need, free });
    }
    Ok(if len > LARGE {
        SizeCheck::Large(len)
    } else {
        SizeCheck::Ok
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn human_plain_bytes() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(2048), "2.0 K");
    }

    #[test]
    fn size_checks_read_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big");
        File::create(&p).unwrap().set_len(LARGE + 1).unwrap();
        assert_eq!(
            check_size_with(&p, 1 << 40).unwrap(),
            SizeCheck::Large(LARGE + 1)
        );
        assert!(matches!(
            check_size_with(&p, 1 << 20),
            Err(Error::OutOfMemory { .. })
        ));
    }
}
