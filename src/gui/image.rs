//! Image decoding with WIC (part of windows): png, jpeg, gif, webp, bmp, ico. Images are
//! decoded straight to the size they are shown at, so a 4000 px photo in an 800 px slot
//! costs 800 px of memory.

use windows::core::GUID;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::*;

fn wic() -> Option<IWICImagingFactory> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()
    }
}

fn frame(f: &IWICImagingFactory, bytes: &[u8]) -> Option<IWICBitmapFrameDecode> {
    unsafe {
        let stream = f.CreateStream().ok()?;
        stream.InitializeFromMemory(bytes).ok()?;
        let dec = f.CreateDecoderFromStream(&stream, std::ptr::null::<GUID>(), WICDecodeMetadataCacheOnDemand).ok()?;
        dec.GetFrame(0).ok()
    }
}

/// Natural size, read from the header.
pub fn size(bytes: &[u8]) -> Option<(u32, u32)> {
    let f = wic()?;
    let fr = frame(&f, bytes)?;
    let (mut w, mut h) = (0u32, 0u32);
    unsafe { fr.GetSize(&mut w, &mut h).ok()? };
    (w > 0 && h > 0).then_some((w, h))
}

/// Premultiplied BGRA pixels no larger than (max_w, max_h), keeping the aspect ratio.
pub fn decode(bytes: &[u8], max_w: u32, max_h: u32) -> Option<(u32, u32, Vec<u8>)> {
    let f = wic()?;
    let fr = frame(&f, bytes)?;
    unsafe {
        let (mut w, mut h) = (0u32, 0u32);
        fr.GetSize(&mut w, &mut h).ok()?;
        if w == 0 || h == 0 {
            return None;
        }
        let conv = f.CreateFormatConverter().ok()?;
        conv.Initialize(&fr, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None::<&IWICPalette>, 0.0, WICBitmapPaletteTypeCustom).ok()?;
        let k = (max_w.max(1) as f32 / w as f32).min(max_h.max(1) as f32 / h as f32).min(1.0);
        let (tw, th) = (((w as f32 * k).round() as u32).max(1), ((h as f32 * k).round() as u32).max(1));
        let mut px = vec![0u8; (tw * th * 4) as usize];
        if tw < w || th < h {
            let sc = f.CreateBitmapScaler().ok()?;
            sc.Initialize(&conv, tw, th, WICBitmapInterpolationModeFant).ok()?;
            sc.CopyPixels(std::ptr::null(), tw * 4, &mut px).ok()?;
        } else {
            conv.CopyPixels(std::ptr::null(), tw * 4, &mut px).ok()?;
        }
        Some((tw, th, px))
    }
}
