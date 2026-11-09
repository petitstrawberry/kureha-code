//! Image resources are decoded off the UI thread, bounded and reused across edits.
use scarlet_ui::buffer::Buffer;
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_DOCUMENT_PIXELS: usize = 16 * 1024 * 1024;
#[derive(Clone)]
pub struct Resource {
    pub result: Result<Arc<Buffer>, String>,
    path: Option<PathBuf>,
    modified: Option<SystemTime>,
    len: u64,
    loaded: Instant,
}

impl Resource {
    pub fn error(message: &str) -> Self {
        Self {
            result: Err(message.into()),
            path: None,
            modified: None,
            len: 0,
            loaded: Instant::now(),
        }
    }
}

/// Decode URI escapes without treating + as a space in a file path.
pub fn decode_path(value: &str) -> Result<String, String> {
    let mut bytes = Vec::new();
    let mut input = value.as_bytes().iter();
    while let Some(&byte) = input.next() {
        if byte == b'%' {
            let a = *input.next().ok_or("Invalid URL escape")?;
            let b = *input.next().ok_or("Invalid URL escape")?;
            let hex = |c: u8| (c as char).to_digit(16).ok_or("Invalid URL escape");
            bytes.push((hex(a)? * 16 + hex(b)?) as u8);
        } else {
            bytes.push(byte);
        }
    }
    String::from_utf8(bytes).map_err(|_| "Invalid UTF-8 path".into())
}
pub fn local_path(destination: &str, base: Option<&Path>) -> Result<PathBuf, String> {
    if destination.contains(':') || destination.starts_with("//") {
        return Err("Unsupported image URL".into());
    }
    let path = decode_path(destination.split(['#', '?']).next().unwrap_or(destination))?;
    let path = PathBuf::from(path);
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(base
            .and_then(Path::parent)
            .ok_or("Save the Markdown file to resolve relative images")?
            .join(path))
    }
}

pub fn load(destination: &str, base: Option<&Path>, previous: Option<&Resource>) -> Resource {
    let remote = destination.starts_with("https://") || destination.starts_with("http://");
    let path = (!remote).then(|| local_path(destination, base)).transpose();
    let path = match path {
        Ok(path) => path,
        Err(error) => {
            return Resource {
                result: Err(error),
                path: None,
                modified: None,
                len: 0,
                loaded: Instant::now(),
            };
        }
    };
    let metadata = path.as_ref().and_then(|p| std::fs::metadata(p).ok());
    let modified = metadata.as_ref().and_then(|m| m.modified().ok());
    let len = metadata.as_ref().map_or(0, |m| m.len());
    if let Some(previous) = previous {
        if previous.path == path
            && previous.modified == modified
            && previous.len == len
            && (!remote || previous.loaded.elapsed() < Duration::from_secs(60))
        {
            return previous.clone();
        }
    }
    let result = (|| {
        let bytes = if remote {
            download(destination)?
        } else {
            let path = path.as_ref().unwrap();
            if !metadata
                .as_ref()
                .is_some_and(|m| m.is_file() && m.len() <= MAX_IMAGE_BYTES)
            {
                return Err("Image missing, not a regular file, or larger than 16 MiB".into());
            }
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .map_err(|e| e.to_string())?
                .take(MAX_IMAGE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            bytes
        };
        if bytes.len() as u64 > MAX_IMAGE_BYTES {
            return Err("Image exceeds 16 MiB".into());
        }
        let asset = crate::media::decode(&bytes)?;
        let mut buffer = Buffer::from_dimensions(asset.bitmap.width(), asset.bitmap.height());
        buffer.as_mut_slice().copy_from_slice(asset.bitmap.pixels());
        Ok(Arc::new(buffer))
    })();
    Resource {
        result,
        path,
        modified,
        len,
        loaded: Instant::now(),
    }
}
#[cfg(not(target_os = "scarlet"))]
fn download(url: &str) -> Result<Vec<u8>, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(4)))
        .max_redirects(3)
        .build()
        .new_agent();
    let mut response = agent.get(url).call().map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}
#[cfg(target_os = "scarlet")]
fn download(_: &str) -> Result<Vec<u8>, String> {
    Err("Remote images are not supported on this host yet".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageEncoder;
    fn png() -> Vec<u8> {
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(
                &[240, 50, 90, 128, 30, 150, 180, 255],
                2,
                1,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        bytes
    }
    #[test]
    fn relative_unicode_images_share_pixels_and_reload_after_replacement() {
        let directory =
            std::env::temp_dir().join(format!("kureha-md-resources-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let base = directory.join("readme.md");
        let path = directory.join("絵 1.png");
        std::fs::write(&path, png()).unwrap();
        let first = load("絵%201.png", Some(&base), None);
        let buffer = first.result.as_ref().unwrap();
        assert_eq!(buffer.as_slice(), &[0x80f0325a, 0xff1e96b4]);
        let cached = load("絵%201.png", Some(&base), Some(&first));
        assert!(Arc::ptr_eq(buffer, cached.result.as_ref().unwrap()));
        std::fs::write(&path, b"broken").unwrap();
        assert!(
            load("絵%201.png", Some(&base), Some(&cached))
                .result
                .is_err()
        );
        assert!(
            load("javascript:alert(1)", Some(&base), None)
                .result
                .is_err()
        );
        assert!(load("missing.png", Some(&base), None).result.is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    #[cfg(not(target_os = "scarlet"))]
    fn http_images_decode_once_and_reuse_the_resource() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/image.png", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = [0; 4096];
            stream.read(&mut request).unwrap();
            let bytes = png();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).unwrap();
            stream.write_all(&bytes).unwrap();
        });
        let first = load(&url, None, None);
        server.join().unwrap();
        let cached = load(&url, None, Some(&first));
        assert!(Arc::ptr_eq(
            first.result.as_ref().unwrap(),
            cached.result.as_ref().unwrap()
        ));
    }
}
