//! Drawing and reading `bitchat://verify` QR codes as pixels, shared by the
//! verify sheet, the webcam scanner and `--qr-lab`.

use std::path::Path;

const MODULE_PX: u32 = 4;
const QUIET_MODULES: u32 = 4;
/// Phone photos are far larger than a QR needs.
const MAX_DECODE_SIDE: u32 = 1600;

/// Black modules on white with a quiet zone, `MODULE_PX` per module.
/// Returns the side length and RGBA pixels.
pub fn qr_rgba(url: &str) -> Option<(u32, Vec<u8>)> {
    let code = qrcode::QrCode::with_error_correction_level(url.as_bytes(), qrcode::EcLevel::M).ok()?;
    let modules = code.width() as u32;
    let colors = code.to_colors();
    let side = (modules + 2 * QUIET_MODULES) * MODULE_PX;
    let mut rgba = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        for x in 0..side {
            let (mx, my) = ((x / MODULE_PX).wrapping_sub(QUIET_MODULES), (y / MODULE_PX).wrapping_sub(QUIET_MODULES));
            let dark = mx < modules && my < modules && colors[(my * modules + mx) as usize] == qrcode::Color::Dark;
            let v = if dark { 0 } else { 255 };
            rgba.extend_from_slice(&[v, v, v, 255]);
        }
    }
    Some((side, rgba))
}

pub fn save_qr_png(url: &str, path: &Path) -> Result<(), String> {
    let (side, rgba) = qr_rgba(url).ok_or("text too long for a QR")?;
    let picture = ::image::RgbaImage::from_raw(side, side, rgba).ok_or("bad QR buffer")?;
    picture.save(path).map_err(|e| format!("could not write {}: {e}", path.display()))
}

pub fn decode_qr_file(path: &Path) -> Result<String, String> {
    let picture = ::image::open(path).map_err(|e| format!("could not read that picture: {e}"))?;
    let picture = if picture.width().max(picture.height()) > MAX_DECODE_SIDE {
        picture.thumbnail(MAX_DECODE_SIDE, MAX_DECODE_SIDE)
    } else {
        picture
    };
    decode_qr_luma(&picture.to_luma8())
}

/// The first bitchat verify QR in a grayscale picture.
pub fn decode_qr_luma(luma: &::image::GrayImage) -> Result<String, String> {
    decode_qr_gray(luma.width() as usize, luma.height() as usize, luma.as_raw())
}

/// [`decode_qr_luma`] over a raw row-major 8-bit buffer (camera frames).
pub fn decode_qr_gray(width: usize, height: usize, pixels: &[u8]) -> Result<String, String> {
    if pixels.len() < width * height {
        return Err("short frame".into());
    }
    let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| pixels[y * width + x]);
    prepared
        .detect_grids()
        .into_iter()
        .filter_map(|grid| grid.decode().ok().map(|(_, content)| content))
        .find(|content| content.starts_with("bitchat://verify"))
        .ok_or_else(|| "no bitchat QR found in that picture".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "bitchat://verify?v=1&noise=ab&sign=cd&nick=bob%20x&ts=1&nonce=AAAA&sig=00";

    #[test]
    fn rendered_qr_decodes_back_to_the_url() {
        let (side, rgba) = qr_rgba(URL).unwrap();
        let luma = ::image::GrayImage::from_fn(side, side, |x, y| ::image::Luma([rgba[((y * side + x) * 4) as usize]]));
        assert_eq!(decode_qr_luma(&luma).as_deref(), Ok(URL));
    }

    #[test]
    fn saved_png_reads_back() {
        let path = std::env::temp_dir().join(format!("witchat-qr-test-{}.png", std::process::id()));
        save_qr_png(URL, &path).unwrap();
        let decoded = decode_qr_file(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(decoded.as_deref(), Ok(URL));
    }

    #[test]
    fn blank_picture_reports_no_qr() {
        let blank = ::image::GrayImage::from_pixel(64, 64, ::image::Luma([255]));
        assert!(decode_qr_luma(&blank).is_err());
        assert!(decode_qr_gray(10, 10, &[0; 5]).is_err());
    }
}
