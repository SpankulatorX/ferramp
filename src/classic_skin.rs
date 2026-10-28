//! The original classic base skin belongs to its owners and is not shipped
//! with Ferramp. On request it is fetched from the Internet Archive and
//! checked against a pinned SHA-256, so the archive cannot hand us anything
//! else.

use std::path::PathBuf;

use crate::gfx::{Bitmap, rgb};

pub const ITEM_PAGE: &str = "archive.org/details/winampskin_base_2_91";
#[cfg(feature = "https")]
const BASE_URL: &str = "https://archive.org/download/winampskin_base_2_91/";
pub const SKIN_FILE: &str = "base-2.91.wsz";
const SKIN_SHA256: &str = "0166fb878cd41de07e1cc51067525ccd3f055b94dc3c227d61124fe528788f6f";
const PREVIEW_FILE: &str = "00_coverscreenshot.png";
const PREVIEW_SHA256: &str = "a1b68ee2750b286b818aad3e2504548afc9a15703486d471ac3b30fa401b015a";

/// Result of a background fetch, delivered to the event loop.
#[derive(Debug)]
pub enum Fetched {
    Preview(Result<Bitmap, String>),
    Skin(Result<PathBuf, String>),
}

#[cfg(feature = "https")]
fn verify(data: &[u8], sha256: &str) -> Result<(), String> {
    let d = ring::digest::digest(&ring::digest::SHA256, data);
    let hex: String = d.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    if hex == sha256 {
        Ok(())
    } else {
        Err("the file does not match the expected checksum".into())
    }
}

/// Downloads one file of the archive item and checks its SHA-256.
#[cfg(feature = "https")]
fn fetch(file: &str, sha256: &str) -> Result<Vec<u8>, String> {
    let data = crate::net::get(&format!("{BASE_URL}{file}"), 4 << 20)?;
    verify(&data, sha256)?;
    Ok(data)
}

#[cfg(not(feature = "https"))]
fn fetch(_file: &str, _sha256: &str) -> Result<Vec<u8>, String> {
    Err("this build has no https support".into())
}

/// The archive's screenshot of the docked windows, reduced to 1x.
pub fn preview() -> Result<Bitmap, String> {
    let data = fetch(PREVIEW_FILE, PREVIEW_SHA256)?;
    decode_preview(&data)
}

fn decode_preview(data: &[u8]) -> Result<Bitmap, String> {
    let img = image::load_from_memory(data)
        .map_err(|e| e.to_string())?
        .to_rgb8();
    // The screenshot is drawn at double size; every other pixel is the 1x image.
    let (w, h) = (img.width() as i32 / 2, img.height() as i32 / 2);
    let mut b = Bitmap::new(w, h, 0xFF00_0000);
    for y in 0..h {
        for x in 0..w {
            let p = img.get_pixel(x as u32 * 2, y as u32 * 2);
            b.set(x, y, rgb(p[0], p[1], p[2]));
        }
    }
    Ok(b)
}

/// Downloads the skin into the skins folder and returns its path.
pub fn download() -> Result<PathBuf, String> {
    let data = fetch(SKIN_FILE, SKIN_SHA256)?;
    let dir = crate::config::skins_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(SKIN_FILE);
    let part = dir.join(format!("{SKIN_FILE}.part"));
    std::fs::write(&part, &data).map_err(|e| e.to_string())?;
    std::fs::rename(&part, &path).map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(all(test, feature = "https"))]
mod tests {
    use super::*;

    #[test]
    fn checksum_mismatch_is_rejected() {
        assert!(verify(b"not the skin", SKIN_SHA256).is_err());
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert!(verify(b"", empty).is_ok());
    }

    #[test]
    fn preview_is_halved() {
        let mut img = image::RgbImage::new(4, 2);
        img.put_pixel(0, 0, image::Rgb([255, 0, 0]));
        img.put_pixel(1, 0, image::Rgb([255, 0, 0]));
        img.put_pixel(2, 0, image::Rgb([0, 0, 255]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let b = decode_preview(&png).unwrap();
        assert_eq!((b.w, b.h), (2, 1));
        assert_eq!(b.get(0, 0), rgb(255, 0, 0));
        assert_eq!(b.get(1, 0), rgb(0, 0, 255));
    }

    /// Fetches the real files; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn archive_files_match_pins() {
        let p = preview().unwrap();
        assert_eq!((p.w, p.h), (275, 348));
        let data = fetch(SKIN_FILE, SKIN_SHA256).unwrap();
        assert!(data.len() > 50_000);
    }
}
