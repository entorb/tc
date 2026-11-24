//! Windows: embed the app icon into tc.exe (Explorer / taskbar preview). macOS / Linux binaries have no icon slot.

#[path = "src/icon.rs"]
mod icon;

fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=src/icon.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default());
    let ico = out.join("tc.ico");
    std::fs::write(&ico, ico_bytes(&icon::floppy_rgba(), icon::N))?;
    let mut res = winresource::WindowsResource::new();
    res.set_icon(&ico.to_string_lossy());
    res.compile()
}

/// One-image .ico: 32-bit BMP (BGRA rows bottom-up, double height) plus an all-zero AND mask.
fn ico_bytes(rgba: &[u8], n: u32) -> Vec<u8> {
    let mask = n.div_ceil(32) * 4 * n;
    let size = 40 + n * n * 4 + mask;
    let mut b = Vec::with_capacity(22 + size as usize);
    // ICONDIR + ICONDIRENTRY (width / height byte 0 means 256)
    for v in [0u16, 1, 1] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    let side = u8::try_from(n % 256).unwrap_or(0);
    b.extend_from_slice(&[side, side, 0, 0]);
    for v in [1u16, 32] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    for v in [size, 22] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    // BITMAPINFOHEADER: size, width, height (image + mask), planes, bpp, compression, image size, 4 unused
    for v in [40, n, 2 * n] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    for v in [1u16, 32] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    for v in [0, size - 40, 0, 0, 0, 0] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    for row in rgba.chunks_exact(n as usize * 4).rev() {
        for &[r, g, bl, a] in row.as_chunks::<4>().0 {
            b.extend_from_slice(&[bl, g, r, a]);
        }
    }
    assert_eq!(b.len(), 62 + (n * n * 4) as usize, "ico header layout");
    b.resize(22 + size as usize, 0); // AND mask: unused, alpha decides
    b
}
