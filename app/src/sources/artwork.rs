//! Cover art helpers shared by the sources: recognizing image bytes, building
//! `data:` URLs and reading `file://` URLs, always within [`MAX_ARTWORK_BYTES`].
//!
//! A window can load `https:`, `http:` and `data:` URLs, so a source hands
//! web URLs over as they are and turns everything local into a `data:` URL.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
#[cfg(any(target_os = "linux", test))]
use std::path::{Path, PathBuf};

/// The largest cover image a source reads: 2 MiB.
#[cfg_attr(
    not(any(target_os = "linux", windows, target_os = "macos")),
    allow(dead_code)
)]
pub(super) const MAX_ARTWORK_BYTES: usize = 2 * 1024 * 1024;

/// The image type of `bytes` from their first bytes: `image/png`,
/// `image/jpeg`, `image/webp` or `image/gif`. Anything else is `None`.
#[cfg_attr(
    not(any(target_os = "linux", windows, target_os = "macos")),
    allow(dead_code)
)]
pub(super) fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.get(..4) == Some(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else {
        None
    }
}

/// `data:<mime>;base64,<bytes>`.
#[cfg_attr(
    not(any(target_os = "linux", windows, target_os = "macos")),
    allow(dead_code)
)]
pub(super) fn data_url(mime: &str, bytes: &[u8]) -> String {
    format!("data:{mime};base64,{}", BASE64.encode(bytes))
}

/// `bytes` as a `data:` URL with the type sniffed from them. `None` when they
/// are not a PNG, JPEG, WebP or GIF image, or larger than [`MAX_ARTWORK_BYTES`].
#[cfg(any(target_os = "linux", windows, test))]
pub(super) fn image_data_url(bytes: &[u8]) -> Option<String> {
    if bytes.len() > MAX_ARTWORK_BYTES {
        return None;
    }
    sniff_image_mime(bytes).map(|mime| data_url(mime, bytes))
}

/// A `data:` URL from base64 image data as players hand it over (ASCII
/// whitespace in it is ignored). The type is sniffed from the bytes; when it
/// is none of the sniffed ones, `mime` is used if it names an image type
/// (e.g. `image/tiff`). `None` when the data is empty, not valid base64,
/// larger than [`MAX_ARTWORK_BYTES`], or of no known image type.
#[cfg(any(target_os = "macos", test))]
pub(super) fn data_url_from_base64(data: &str, mime: Option<&str>) -> Option<String> {
    // 2 MiB take this many base64 characters; twice as many leave room for
    // line breaks. Anything longer is too big, without decoding it.
    let max_encoded = MAX_ARTWORK_BYTES.div_ceil(3) * 4;
    if data.len() > 2 * max_encoded {
        return None;
    }
    let compact: String;
    let data = if data.bytes().any(|b| b.is_ascii_whitespace()) {
        compact = data.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        compact.as_str()
    } else {
        data
    };
    let bytes = BASE64.decode(data).ok()?;
    if bytes.is_empty() || bytes.len() > MAX_ARTWORK_BYTES {
        return None;
    }
    let mime = match sniff_image_mime(&bytes) {
        Some(sniffed) => sniffed.to_string(),
        None => image_mime(mime?)?,
    };
    Some(data_url(&mime, &bytes))
}

/// `mime` lowercased when it is a plain image type (`image/` followed by
/// letters, digits, `+`, `-` or `.`, without parameters). SVG is refused: it
/// is a document that can carry scripts, not just a picture.
#[cfg(any(target_os = "macos", test))]
fn image_mime(mime: &str) -> Option<String> {
    let mime = mime.trim().to_ascii_lowercase();
    let subtype = mime.strip_prefix("image/")?;
    let valid = !subtype.is_empty()
        && !subtype.starts_with("svg")
        && subtype
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'));
    valid.then_some(mime)
}

/// Cover art from a URL a player reported, as a window can load it:
/// `https:` and `http:` URLs as they are, `file:` URLs read with
/// [`read_image_file`]. Any other scheme gives `None`.
#[cfg(any(target_os = "linux", test))]
pub(super) async fn artwork_from_url(url: &str) -> anyhow::Result<Option<String>> {
    let url = url.trim();
    let has_scheme = |scheme: &str| super::strip_prefix_ignore_ascii_case(url, scheme).is_some();
    if has_scheme("https://") || has_scheme("http://") {
        return Ok(Some(url.to_string()));
    }
    if has_scheme("file:") {
        let path =
            file_url_path(url).ok_or_else(|| anyhow::anyhow!("not a local file URL: {url}"))?;
        return read_image_file(&path).await;
    }
    Ok(None)
}

/// The local path of a `file:` URL, percent-decoded: `file:///path` or
/// `file://localhost/path`. A query or fragment is dropped. `None` for any
/// other form (another host, a relative path), a bad percent escape, or an
/// encoded NUL.
#[cfg(any(target_os = "linux", test))]
pub(super) fn file_url_path(url: &str) -> Option<PathBuf> {
    let rest = super::strip_prefix_ignore_ascii_case(url.trim(), "file://")?;
    let rest = match rest.find('/') {
        Some(0) => rest,
        Some(slash) if rest[..slash].eq_ignore_ascii_case("localhost") => &rest[slash..],
        _ => return None,
    };
    let path = rest.split(['?', '#']).next().unwrap_or("");
    let bytes = percent_decode(path)?;
    if bytes.contains(&0) {
        return None;
    }
    path_from_bytes(bytes)
}

/// Decodes `%XX` escapes. `None` when a `%` is not followed by two hex digits.
#[cfg(any(target_os = "linux", test))]
fn percent_decode(text: &str) -> Option<Vec<u8>> {
    let mut bytes = text.bytes();
    let mut decoded = Vec::with_capacity(text.len());
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = hex_value(bytes.next()?)?;
            let low = hex_value(bytes.next()?)?;
            decoded.push((high << 4) | low);
        } else {
            decoded.push(byte);
        }
    }
    Some(decoded)
}

#[cfg(any(target_os = "linux", test))]
fn hex_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

/// Paths are bytes on Unix, so any decoded name works.
#[cfg(all(unix, any(target_os = "linux", test)))]
fn path_from_bytes(bytes: Vec<u8>) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// Elsewhere only UTF-8 names are accepted.
#[cfg(all(not(unix), test))]
fn path_from_bytes(bytes: Vec<u8>) -> Option<PathBuf> {
    String::from_utf8(bytes).ok().map(PathBuf::from)
}

/// Reads the image at `path` as a `data:` URL (see [`image_data_url`]).
/// Errors when it is not a regular file, cannot be read, or is larger than
/// [`MAX_ARTWORK_BYTES`]; `None` when it is not a PNG, JPEG, WebP or GIF image.
#[cfg(any(target_os = "linux", test))]
pub(super) async fn read_image_file(path: &Path) -> anyhow::Result<Option<String>> {
    use anyhow::Context;
    use tokio::io::AsyncReadExt;

    let too_big = || {
        anyhow::anyhow!(
            "the cover art {} is larger than {} MiB",
            path.display(),
            MAX_ARTWORK_BYTES / (1024 * 1024)
        )
    };
    // Checked first: opening a pipe or a device could block or never end.
    let meta = tokio::fs::metadata(path)
        .await
        .with_context(|| format!("could not read the cover art {}", path.display()))?;
    if !meta.is_file() {
        anyhow::bail!("the cover art {} is not a file", path.display());
    }
    if meta.len() > MAX_ARTWORK_BYTES as u64 {
        return Err(too_big());
    }
    let file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("could not open the cover art {}", path.display()))?;
    let mut bytes = Vec::new();
    // One byte more than allowed, to notice a file that grew since.
    file.take(MAX_ARTWORK_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .with_context(|| format!("could not read the cover art {}", path.display()))?;
    if bytes.len() > MAX_ARTWORK_BYTES {
        return Err(too_big());
    }
    Ok(image_data_url(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10, b'J', b'F', b'I', b'F'];
    const WEBP: &[u8] = b"RIFF\x24\0\0\0WEBPVP8 ";
    const GIF: &[u8] = b"GIF89a\x01\0\x01\0";

    // ---- sniffing and data: URLs ---------------------------------------------

    #[test]
    fn sniffs_the_four_image_types() {
        assert_eq!(sniff_image_mime(PNG), Some("image/png"));
        assert_eq!(sniff_image_mime(JPEG), Some("image/jpeg"));
        assert_eq!(sniff_image_mime(WEBP), Some("image/webp"));
        assert_eq!(sniff_image_mime(GIF), Some("image/gif"));
        assert_eq!(sniff_image_mime(b"GIF87a"), Some("image/gif"));
    }

    #[test]
    fn anything_else_is_not_an_image() {
        for bytes in [
            &b""[..],
            b"\x89PNG",
            &[0xFF, 0xD8],
            b"RIFF\x24\0\0\0WAVEfmt ",
            b"RIFF",
            b"GIF90a",
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
            b"BM\x36\0\0\0",
            b"II*\0",
            b"\0\0\0\x18ftypheic",
        ] {
            assert_eq!(sniff_image_mime(bytes), None, "{bytes:?}");
        }
    }

    #[test]
    fn data_urls() {
        assert_eq!(
            data_url("image/gif", b"GIF89a"),
            "data:image/gif;base64,R0lGODlh"
        );
        assert_eq!(data_url("image/png", b""), "data:image/png;base64,");
        assert_eq!(
            image_data_url(PNG).as_deref(),
            Some("data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==")
        );
        assert_eq!(image_data_url(b"plain text"), None);
        assert_eq!(image_data_url(b""), None);
    }

    #[test]
    fn image_data_url_size_limit() {
        let mut bytes = JPEG.to_vec();
        bytes.resize(MAX_ARTWORK_BYTES, 0);
        let url = image_data_url(&bytes).expect("exactly 2 MiB is fine");
        assert!(
            url.starts_with("data:image/jpeg;base64,/9j/4"),
            "{}",
            &url[..40]
        );
        bytes.push(0);
        assert_eq!(image_data_url(&bytes), None);
    }

    // ---- base64 from a player --------------------------------------------------

    #[test]
    fn base64_with_a_sniffed_type() {
        let data = BASE64.encode(PNG);
        let expected = Some(data_url("image/png", PNG));
        assert_eq!(data_url_from_base64(&data, Some("image/png")), expected);
        assert_eq!(data_url_from_base64(&data, None), expected);
        // The bytes win over a wrong type.
        assert_eq!(data_url_from_base64(&data, Some("image/jpeg")), expected);
        assert_eq!(data_url_from_base64(&data, Some("text/html")), expected);
        // Line breaks and spaces, as some encoders write them.
        let wrapped = format!(" {}\r\n{} ", &data[..8], &data[8..]);
        assert_eq!(data_url_from_base64(&wrapped, None), expected);
    }

    #[test]
    fn base64_of_another_image_type_uses_the_given_type() {
        let tiff = b"II*\0\x08\0\0\0";
        let data = BASE64.encode(tiff);
        assert_eq!(
            data_url_from_base64(&data, Some(" Image/TIFF ")),
            Some(data_url("image/tiff", tiff))
        );
        assert_eq!(
            data_url_from_base64(&data, Some("image/heic")),
            Some(data_url("image/heic", tiff))
        );
        for mime in [
            None,
            Some(""),
            Some("image/svg+xml"),
            Some("image/"),
            Some("text/plain"),
            Some("image/png; charset=x"),
            Some("image/png\"><script>"),
            Some("application/octet-stream"),
        ] {
            assert_eq!(data_url_from_base64(&data, mime), None, "{mime:?}");
        }
    }

    #[test]
    fn bad_base64_is_nothing() {
        for data in ["", "   ", "not base64!", "iVBORw0KGgo", "====", "%%%%"] {
            assert_eq!(
                data_url_from_base64(data, Some("image/png")),
                None,
                "{data:?}"
            );
        }
    }

    #[test]
    fn base64_size_limit() {
        let mut bytes = GIF.to_vec();
        bytes.resize(MAX_ARTWORK_BYTES, 0);
        let data = BASE64.encode(&bytes);
        let url = data_url_from_base64(&data, None).expect("exactly 2 MiB is fine");
        assert!(url.starts_with("data:image/gif;base64,R0lGOD"));
        bytes.push(0);
        assert_eq!(data_url_from_base64(&BASE64.encode(&bytes), None), None);
        // Far too long is refused before decoding (and it is not even base64).
        assert_eq!(
            data_url_from_base64(&"!".repeat(4 * MAX_ARTWORK_BYTES), None),
            None
        );
    }

    // ---- file: URLs --------------------------------------------------------------

    #[test]
    fn file_urls_to_paths() {
        let path = |url: &str| file_url_path(url).map(|p| p.to_string_lossy().into_owned());
        assert_eq!(
            path("file:///tmp/cover.png").as_deref(),
            Some("/tmp/cover.png")
        );
        assert_eq!(
            path("FILE://localhost/tmp/cover.png").as_deref(),
            Some("/tmp/cover.png")
        );
        assert_eq!(path("file://LocalHost/tmp/a").as_deref(), Some("/tmp/a"));
        assert_eq!(
            path("file:///home/me/Music/Bj%C3%B6rk%20-%20Homogenic/cover%2Ejpg").as_deref(),
            Some("/home/me/Music/Björk - Homogenic/cover.jpg")
        );
        assert_eq!(
            path("file:///tmp/a%23b%3Fc.png?size=large#x").as_deref(),
            Some("/tmp/a#b?c.png")
        );
        assert_eq!(path("  file:///tmp/x.png\n").as_deref(), Some("/tmp/x.png"));
    }

    #[test]
    fn file_urls_that_are_not_local_paths() {
        for url in [
            "",
            "/tmp/cover.png",
            "file:/tmp/cover.png",
            "file://",
            "file://server/share/cover.png",
            "file://localhost",
            "file:///tmp/%",
            "file:///tmp/%4",
            "file:///tmp/%zz.png",
            "file:///tmp/a%00b.png",
            "https://example.com/cover.png",
        ] {
            assert_eq!(file_url_path(url), None, "{url:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn file_urls_keep_names_that_are_not_utf8() {
        use std::os::unix::ffi::OsStrExt;
        let path = file_url_path("file:///tmp/caf%E9.png").unwrap();
        assert_eq!(path.as_os_str().as_bytes(), b"/tmp/caf\xe9.png");
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%2fb%2F").as_deref(), Some(&b"a/b/"[..]));
        assert_eq!(percent_decode("%E2%99%AB").as_deref(), Some("♫".as_bytes()));
        assert_eq!(percent_decode("100%").as_deref(), None);
        assert_eq!(percent_decode("%g0").as_deref(), None);
        assert_eq!(
            percent_decode("plain+text").as_deref(),
            Some(&b"plain+text"[..])
        );
        assert_eq!(percent_decode("♫").as_deref(), Some("♫".as_bytes()));
    }

    // ---- reading files -------------------------------------------------------------

    #[tokio::test]
    async fn reads_an_image_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cover.webp");
        std::fs::write(&path, WEBP).unwrap();
        assert_eq!(
            read_image_file(&path).await.unwrap(),
            Some(data_url("image/webp", WEBP))
        );

        let text = dir.path().join("cover.txt");
        std::fs::write(&text, "not an image").unwrap();
        assert_eq!(read_image_file(&text).await.unwrap(), None);
    }

    #[tokio::test]
    async fn files_that_cannot_be_cover_art_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let missing = read_image_file(&dir.path().join("missing.png")).await;
        assert!(missing.is_err());
        let folder = read_image_file(dir.path()).await.unwrap_err();
        assert!(folder.to_string().contains("not a file"), "{folder}");

        let big = dir.path().join("big.png");
        let mut bytes = PNG.to_vec();
        bytes.resize(MAX_ARTWORK_BYTES + 1, 0);
        std::fs::write(&big, &bytes).unwrap();
        let err = read_image_file(&big).await.unwrap_err();
        assert!(err.to_string().contains("larger than 2 MiB"), "{err}");

        bytes.truncate(MAX_ARTWORK_BYTES);
        std::fs::write(&big, &bytes).unwrap();
        assert!(read_image_file(&big).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn urls_from_players() {
        assert_eq!(
            artwork_from_url(" https://i.scdn.co/image/ab67616d0000b273 ")
                .await
                .unwrap()
                .as_deref(),
            Some("https://i.scdn.co/image/ab67616d0000b273")
        );
        assert_eq!(
            artwork_from_url("HTTP://example.com/a.jpg")
                .await
                .unwrap()
                .as_deref(),
            Some("HTTP://example.com/a.jpg")
        );
        for url in [
            "",
            "data:image/png;base64,iVBORw0KGgo=",
            "ftp://example.com/a.png",
            "/tmp/cover.png",
            "javascript:alert(1)",
        ] {
            assert_eq!(artwork_from_url(url).await.unwrap(), None, "{url:?}");
        }
        assert!(artwork_from_url("file://server/a.png").await.is_err());
        assert!(artwork_from_url("file:///definitely/not/here.png")
            .await
            .is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_urls_from_players_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Cover Art #1.jpg");
        std::fs::write(&path, JPEG).unwrap();
        let encoded = path
            .to_str()
            .unwrap()
            .replace('%', "%25")
            .replace(' ', "%20")
            .replace('#', "%23");
        assert_eq!(
            artwork_from_url(&format!("file://{encoded}"))
                .await
                .unwrap(),
            Some(data_url("image/jpeg", JPEG))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_pipe_is_not_opened() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("fifo");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !made {
            return;
        }
        // Opening it for reading would wait for a writer forever.
        let err = tokio::time::timeout(std::time::Duration::from_secs(5), read_image_file(&fifo))
            .await
            .expect("returns at once")
            .unwrap_err();
        assert!(err.to_string().contains("not a file"), "{err}");
    }
}
