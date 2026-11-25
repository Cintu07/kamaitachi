//! Renders a page with the Android engine on a computer and writes it as a BMP, to
//! check text, layout and drawing without a phone:
//!
//!     KAMAITACHI_FONTS=/path/to/fonts cargo run --release --example shot -- https://news.ycombinator.com out.bmp
//!
//! The folder needs Android's font file names (Roboto-Regular.ttf, NotoSerif-Regular.ttf,
//! DroidSansMono.ttf); any fonts will do under those names.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (url, out) = (args.get(1).cloned().unwrap_or_else(|| "https://example.com".into()), args.get(2).cloned().unwrap_or_else(|| "shot.bmp".into()));
    // a phone: 1080 px wide at 2.75 device px per css px
    let (w, h, dpr) = (1080u32, 2200u32, 2.75f32);
    let t = std::time::Instant::now();
    let px = kamaitachi::shot(&url, w, h, dpr);
    eprintln!("{url}: {} ms", t.elapsed().as_millis());
    // the frame is RGBA in memory, as in a Bitmap; a BMP wants BGRA, bottom row first
    let mut f = Vec::with_capacity(54 + px.len() * 4);
    let size = 54 + px.len() as u32 * 4;
    f.extend_from_slice(b"BM");
    for v in [size, 0, 54, 40, w, (h as i32).wrapping_neg() as u32] {
        f.extend_from_slice(&v.to_le_bytes());
    }
    f.extend_from_slice(&1u16.to_le_bytes());
    f.extend_from_slice(&32u16.to_le_bytes());
    for v in [0u32, px.len() as u32 * 4, 2835, 2835, 0, 0] {
        f.extend_from_slice(&v.to_le_bytes());
    }
    for p in px {
        let [r, g, b, a] = p.to_le_bytes();
        f.extend_from_slice(&[b, g, r, a]);
    }
    std::fs::write(out, f).expect("write");
}
