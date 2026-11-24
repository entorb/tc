//! App icon pixels, shared by the window icon (`main.rs`) and the Windows .exe icon (`build.rs`).

/// Icon edge in pixels.
pub const N: u32 = 64;

/// A green floppy disk as `N`×`N` RGBA, top row first; drawn in code (no image asset, no decoder).
pub fn floppy_rgba() -> Vec<u8> {
    const BODY: [u8; 4] = [30, 120, 60, 255];
    let mut rgba = Vec::with_capacity((N * N * 4) as usize);
    for y in 0..N {
        for x in 0..N {
            let inside = (4..60).contains(&x) && (4..60).contains(&y) && x + (63 - y) < 112;
            let shutter = (18..46).contains(&x) && (4..24).contains(&y);
            let slot = (36..42).contains(&x) && (8..20).contains(&y);
            let label = (12..52).contains(&x) && (32..60).contains(&y);
            let line = label && y > 36 && y % 6 == 0 && (16..48).contains(&x);
            let px: [u8; 4] = match () {
                () if !inside => [0, 0, 0, 0],
                () if slot => BODY,
                () if shutter => [200, 205, 215, 255],
                () if line => [120, 130, 150, 255],
                () if label => [245, 245, 240, 255],
                () => BODY,
            };
            rgba.extend_from_slice(&px);
        }
    }
    rgba
}
