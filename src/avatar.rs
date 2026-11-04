//! The bot's avatar: an image from the config (an HTTPS URL or a base64 string) that becomes
//! the bot's standard Mumble user texture, so every client shows it.
//!
//! The image is loaded once at startup. It is decoded to prove it is a real PNG or JPEG, scaled
//! down to Mumble's texture limits and encoded again, so only a clean image ever reaches the
//! server. A failure is reported as a reason and the bot runs without an avatar; the load is
//! tried again only on the next start.

use std::io::Cursor;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat, ImageReader, Limits};

use crate::music::UrlConfig;
use crate::music::source::{SourceUrl, fetch_as};

/// Mumble's texture limits: clients scale avatars to at most 600 x 60 pixels.
const MAX_WIDTH: u32 = 600;
const MAX_HEIGHT: u32 = 60;
/// The standard server limit for an image message (`imagemessagelength`, 128 KiB).
const MAX_TEXTURE_BYTES: usize = 128 * 1024;
/// Largest download or decoded base64 input.
const MAX_INPUT_BYTES: usize = 2 * 1024 * 1024;
/// Largest source image that is decoded: guards against small files that unpack to huge bitmaps.
const MAX_SOURCE_SIDE: u32 = 8192;
const FETCH_TIMEOUT_SECS: u64 = 10;

/// Where the config says the avatar comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AvatarSource {
    Url(String),
    Base64(Vec<u8>),
}

impl AvatarSource {
    /// Reads the config value: `https://...` is a URL, anything else must be base64. A local
    /// path or a plain `http` URL is an error, so a misconfiguration shows at startup.
    pub fn parse(value: &str) -> Result<Self> {
        let value = value.trim();
        if value.is_empty() {
            bail!("avatar is empty");
        }
        if value.starts_with("https://") {
            return Ok(Self::Url(value.to_owned()));
        }
        if value.contains("://") {
            bail!("avatar URLs must use https");
        }
        let compact: String = value.chars().filter(|c| !c.is_whitespace()).collect();
        let bytes = STANDARD.decode(compact).context(
            "avatar is neither an https URL nor valid base64 (local paths are not supported)",
        )?;
        Ok(Self::Base64(bytes))
    }
}

/// Which addresses and schemes a URL may use. Production only allows public https hosts; the
/// tests need a local plain-HTTP server.
#[derive(Debug, Clone, Copy)]
struct Policy {
    allow_local_http: bool,
}

/// Loads, checks and scales the avatar; the result is ready for `UserState.texture`.
pub async fn load(source: &AvatarSource) -> Result<Vec<u8>> {
    load_with(
        source,
        Policy {
            allow_local_http: false,
        },
    )
    .await
}

async fn load_with(source: &AvatarSource, policy: Policy) -> Result<Vec<u8>> {
    let raw = match source {
        AvatarSource::Base64(bytes) => {
            if bytes.len() > MAX_INPUT_BYTES {
                bail!(
                    "image is larger than {} MiB",
                    MAX_INPUT_BYTES / (1024 * 1024)
                );
            }
            bytes.clone()
        }
        AvatarSource::Url(url) => download(url, policy).await?,
    };
    tokio::task::spawn_blocking(move || prepare(&raw)).await?
}

async fn download(url: &str, policy: Policy) -> Result<Vec<u8>> {
    let parsed = SourceUrl::parse_any(url).map_err(|e| anyhow::anyhow!(e.reason()))?;
    if !parsed.tls && !policy.allow_local_http {
        bail!("avatar URLs must use https");
    }
    let urls = UrlConfig {
        enabled: true,
        allow_hosts: Vec::new(),
        allow_private: policy.allow_local_http,
        timeout_secs: FETCH_TIMEOUT_SECS,
    };
    // `reason` never repeats the URL, which may carry a secret in its query.
    fetch_as(&parsed, &urls, MAX_INPUT_BYTES, "image/png, image/jpeg")
        .await
        .map_err(|e| anyhow::anyhow!(e.reason()))
}

/// Proves the bytes are a PNG or JPEG by decoding them, then scales and encodes them again.
fn prepare(raw: &[u8]) -> Result<Vec<u8>> {
    let mut reader = ImageReader::new(Cursor::new(raw))
        .with_guessed_format()
        .context("cannot read the image")?;
    match reader.format() {
        Some(ImageFormat::Png | ImageFormat::Jpeg) => {}
        _ => bail!("image is not a PNG or JPEG"),
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_SIDE);
    limits.max_image_height = Some(MAX_SOURCE_SIDE);
    reader.limits(limits);
    let image = reader.decode().context("image cannot be decoded")?;
    let image = if image.width() > MAX_WIDTH || image.height() > MAX_HEIGHT {
        image.resize(MAX_WIDTH, MAX_HEIGHT, FilterType::Lanczos3)
    } else {
        image
    };
    encode(&image)
}

/// PNG keeps transparency; a photo that is too big as PNG falls back to JPEG.
fn encode(image: &DynamicImage) -> Result<Vec<u8>> {
    let mut png = Cursor::new(Vec::new());
    image.write_to(&mut png, ImageFormat::Png)?;
    let png = png.into_inner();
    if png.len() <= MAX_TEXTURE_BYTES {
        return Ok(png);
    }
    let mut jpeg = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(image.to_rgb8()).write_to(&mut jpeg, ImageFormat::Jpeg)?;
    let jpeg = jpeg.into_inner();
    if jpeg.len() > MAX_TEXTURE_BYTES {
        bail!(
            "image is still larger than {} KiB after scaling",
            MAX_TEXTURE_BYTES / 1024
        );
    }
    Ok(jpeg)
}

/// Loads the avatar, or logs the reason and returns none: the bot runs without one.
pub async fn load_or_warn(source: &AvatarSource) -> Option<Vec<u8>> {
    match load(source).await {
        Ok(texture) => Some(texture),
        Err(reason) => {
            tracing::warn!(%reason, "avatar not set, running without one until the next start");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const LOCAL: Policy = Policy {
        allow_local_http: true,
    };

    fn encoded(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
        let image = ImageBuffer::from_fn(width, height, |x, y| {
            Rgb([(x % 251) as u8, (y % 241) as u8, ((x * y) % 255) as u8])
        });
        let mut out = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(image)
            .write_to(&mut out, format)
            .expect("encode test image");
        out.into_inner()
    }

    fn size_of(bytes: &[u8]) -> (u32, u32) {
        let image = image::load_from_memory(bytes).expect("result decodes");
        (image.width(), image.height())
    }

    /// Serves `body` with `status` once per connection, on a free local port.
    async fn serve(status: &'static str, body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = stream.read(&mut buf).await;
                    let head = format!(
                        "HTTP/1.0 {status}\r\nContent-Length: {}\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes()).await;
                    let _ = stream.write_all(&body).await;
                });
            }
        });
        format!("http://127.0.0.1:{port}/avatar")
    }

    #[test]
    fn valid_png_and_jpeg_are_kept_when_small() {
        for format in [ImageFormat::Png, ImageFormat::Jpeg] {
            let out = prepare(&encoded(40, 30, format)).expect("valid image");
            assert_eq!(size_of(&out), (40, 30));
        }
    }

    #[test]
    fn large_images_are_scaled_into_the_texture_limits() {
        let out = prepare(&encoded(1200, 400, ImageFormat::Png)).expect("valid image");
        let (width, height) = size_of(&out);
        assert!(
            width <= MAX_WIDTH && height <= MAX_HEIGHT,
            "{width}x{height}"
        );
        assert!(out.len() <= MAX_TEXTURE_BYTES);
    }

    #[test]
    fn invalid_bytes_are_refused() {
        assert!(prepare(b"definitely not an image").is_err());
        let mut truncated = encoded(200, 200, ImageFormat::Png);
        truncated.truncate(80);
        assert!(prepare(&truncated).is_err());
    }

    #[test]
    fn other_image_formats_are_refused() {
        assert!(prepare(b"GIF89a\x01\x00\x01\x00\x00\x00\x00;").is_err());
    }

    #[test]
    fn config_values_are_told_apart() {
        assert!(matches!(
            AvatarSource::parse("https://example.com/a.png"),
            Ok(AvatarSource::Url(_))
        ));
        assert!(AvatarSource::parse("http://example.com/a.png").is_err());
        assert!(AvatarSource::parse("/home/me/a.png").is_err());
        assert!(AvatarSource::parse("  ").is_err());
        let encoded = STANDARD.encode(encoded(8, 8, ImageFormat::Png));
        assert!(matches!(
            AvatarSource::parse(&encoded),
            Ok(AvatarSource::Base64(_))
        ));
    }

    #[tokio::test]
    async fn base64_is_decoded_and_checked() {
        let good = AvatarSource::Base64(encoded(64, 64, ImageFormat::Png));
        let out = load_with(&good, LOCAL).await.expect("base64 avatar");
        assert_eq!(size_of(&out), (60, 60));
        let bad = AvatarSource::Base64(b"plain text".to_vec());
        assert!(load_with(&bad, LOCAL).await.is_err());
    }

    #[tokio::test]
    async fn an_image_is_fetched_from_a_url() {
        let url = serve("200 OK", encoded(300, 300, ImageFormat::Jpeg)).await;
        let out = load_with(&AvatarSource::Url(url), LOCAL)
            .await
            .expect("downloaded avatar");
        assert_eq!(size_of(&out), (60, 60));
    }

    #[tokio::test]
    async fn bad_urls_and_answers_fail() {
        // Plain http and private addresses are refused in production.
        let local = serve("200 OK", encoded(10, 10, ImageFormat::Png)).await;
        assert!(load(&AvatarSource::Url(local.clone())).await.is_err());
        let https_local = local.replace("http://", "https://");
        assert!(load(&AvatarSource::Url(https_local)).await.is_err());
        // Wrong status, not an image, and nothing listening.
        let missing = serve("404 Not Found", Vec::new()).await;
        assert!(load_with(&AvatarSource::Url(missing), LOCAL).await.is_err());
        let text = serve("200 OK", b"<html>no</html>".to_vec()).await;
        assert!(load_with(&AvatarSource::Url(text), LOCAL).await.is_err());
        let closed = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = closed.local_addr().expect("addr").port();
        drop(closed);
        let dead = AvatarSource::Url(format!("http://127.0.0.1:{port}/a.png"));
        assert!(load_with(&dead, LOCAL).await.is_err());
        assert!(
            load_with(&AvatarSource::Url("http://[bad".to_owned()), LOCAL)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn oversized_downloads_are_refused() {
        let url = serve("200 OK", vec![0u8; MAX_INPUT_BYTES + 1]).await;
        let reason = load_with(&AvatarSource::Url(url), LOCAL)
            .await
            .expect_err("too large");
        assert!(
            reason.to_string().to_lowercase().contains("large"),
            "{reason}"
        );
    }

    #[test]
    fn images_with_huge_dimensions_are_refused() {
        // A tiny PNG header that claims a 20000 x 20000 bitmap.
        let mut png = encoded(8, 8, ImageFormat::Png);
        png[16..20].copy_from_slice(&20000u32.to_be_bytes());
        png[20..24].copy_from_slice(&20000u32.to_be_bytes());
        assert!(prepare(&png).is_err());
    }
}
