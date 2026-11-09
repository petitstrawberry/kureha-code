//! Bounded image decoding, performed by the file worker rather than the UI.
use scarlet_ui::views::BitmapImage;
use std::{
    fs::File,
    io::{Cursor, Read},
    path::Path,
};

pub const MAX_ENCODED: u64 = 32 * 1024 * 1024;
const MAX_PIXELS: u64 = 16 * 1024 * 1024;

#[derive(Clone)]
pub struct ImageAsset {
    pub bitmap: BitmapImage,
    pub format: String,
    pub bytes: usize,
}

pub fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico"
            )
        })
}

pub fn read(path: &Path) -> Result<ImageAsset, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Choose a regular image file".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_ENCODED + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    decode(&bytes)
}

pub fn decode(bytes: &[u8]) -> Result<ImageAsset, String> {
    if bytes.len() as u64 > MAX_ENCODED {
        return Err("Image exceeds the 32 MiB file limit".into());
    }
    let reader = || {
        image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|e| e.to_string())
    };
    let format = reader()?.format().ok_or("Unsupported image format")?;
    let (width, height) = reader()?.into_dimensions().map_err(|e| e.to_string())?;
    if width == 0 || height == 0 || width as u64 * height as u64 > MAX_PIXELS {
        return Err("Image exceeds the 16 megapixel preview limit".into());
    }
    let mut decoder = reader()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(128 * 1024 * 1024);
    decoder.limits(limits);
    let rgba = decoder.decode().map_err(|e| e.to_string())?.into_rgba8();
    let pixels = rgba
        .pixels()
        .map(|p| ((p[3] as u32) << 24) | ((p[0] as u32) << 16) | ((p[1] as u32) << 8) | p[2] as u32)
        .collect();
    Ok(ImageAsset {
        bitmap: BitmapImage::from_bgra(pixels, width, height),
        format: format!("{format:?}"),
        bytes: bytes.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageEncoder;
    #[test]
    fn decodes_png_pixels_and_alpha_without_treating_binary_as_text() {
        let mut encoded = Vec::new();
        image::codecs::png::PngEncoder::new(&mut encoded)
            .write_image(
                &[255, 0, 0, 128, 0, 255, 0, 255],
                2,
                1,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        let asset = decode(&encoded).unwrap();
        assert_eq!((asset.bitmap.width(), asset.bitmap.height()), (2, 1));
        assert_eq!(asset.bitmap.pixels(), &[0x80ff0000, 0xff00ff00]);
        assert!(decode(b"not an image").is_err());
        assert!(is_image(Path::new("PHOTO.PNG")));
    }
}
