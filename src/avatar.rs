//! The bot's avatar: the `[bot]` section of the config (Mumcord issue #460).
//!
//! The image comes from an https URL fetched once at startup, or from base64 in the config, and is
//! checked by content. It is scaled to at most `MAX_EDGE` pixels and always re-encoded, never
//! passed through: PNG first, JPEG at falling quality when that does not fit the server's image
//! limit. The bot sends the result as its Mumble user texture after every login, so any standard
//! client shows it without a change on the server side.
//!
//! A missing setting is not reported at all. A problem (bad URL, failed download, no image, too
//! large even after re-encoding, refused by the server) is one warning with the reason, and the bot
//! connects without an avatar: an avatar never stops the bot.

pub mod fetch;

use std::io::Cursor;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageFormat, ImageReader, Limits};
use serde::Deserialize;

use crate::Bot;

/// Largest avatar edge in pixels, the size Mumble clients show.
const MAX_EDGE: u32 = 128;
/// murmur's default `imagemessagelength`, used when the server announces 0 or nothing.
pub const DEFAULT_LIMIT: usize = 131_072;
/// Cap of one avatar download.
const MAX_DOWNLOAD_BYTES: usize = 5_000_000;
/// Time the one download at startup may take, all redirects together.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Pixel edge refused before decoding: a small file can declare dimensions that expand to
/// hundreds of megabytes.
const MAX_DECODE_EDGE: u32 = 4000;
/// Memory the decoder may use for one image, on top of the dimension cap.
const MAX_DECODE_ALLOC: u64 = 100 * 1024 * 1024;
/// Qualities tried when the PNG does not fit the server's limit.
const JPEG_QUALITIES: [u8; 3] = [85, 70, 50];

/// Environment variables for the two settings; they win over the config file, as the peer API
/// secret does.
const ENV_URL: &str = "MUMBOT_AVATAR_URL";
const ENV_BASE64: &str = "MUMBOT_AVATAR_BASE64";

/// The `[bot]` section: one image source, as a URL or as base64.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AvatarConfig {
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub avatar_base64: Option<String>,
}

/// One resolved image source.
#[derive(Debug, PartialEq)]
pub enum Source {
    Url(String),
    Base64(String),
    None,
}

impl AvatarConfig {
    /// The source after the environment overlay: an environment variable wins over the config
    /// file, and an empty value counts as unset.
    pub fn source(&self) -> Result<Source> {
        self.source_from(env_value)
    }

    /// Setting both is a config error at startup.
    pub fn validate(&self) -> Result<()> {
        self.source().map(|_| ())
    }

    fn source_from(&self, env: impl Fn(&str) -> Option<String>) -> Result<Source> {
        let url = picked(ENV_URL, self.avatar_url.as_deref(), &env);
        let base64 = picked(ENV_BASE64, self.avatar_base64.as_deref(), &env);
        match (url, base64) {
            (Some(_), Some(_)) => Err(anyhow!(
                "avatar: set only one of {ENV_URL} / avatar_url and {ENV_BASE64} / avatar_base64"
            )),
            (Some(url), None) => Ok(Source::Url(url)),
            (None, Some(base64)) => Ok(Source::Base64(base64)),
            (None, None) => Ok(Source::None),
        }
    }
}

fn picked(
    name: &str,
    from_config: Option<&str>,
    env: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    env(name).or_else(|| {
        from_config
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// One image, decoded and scaled to the avatar size, ready to send after every login.
#[derive(Debug, Clone, PartialEq)]
pub struct Avatar {
    image: DynamicImage,
}

/// Reads the configured image once at startup. `Ok(None)` is the missing setting; an `Err` is a
/// problem the caller reports as one warning, because an avatar never stops the bot.
pub async fn prepare(config: &AvatarConfig) -> Result<Option<Avatar>> {
    let bytes = match config.source()? {
        Source::None => return Ok(None),
        Source::Url(url) => fetch::get(&url, MAX_DOWNLOAD_BYTES, FETCH_TIMEOUT).await?,
        Source::Base64(text) => STANDARD
            .decode(text)
            .context("avatar_base64 is not base64")?,
    };
    Ok(Some(decode(&bytes)?))
}

/// Decodes by content, not by file name or Content-Type, and scales to `MAX_EDGE`.
fn decode(bytes: &[u8]) -> Result<Avatar> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .context("reading the image failed")?;
    if !matches!(
        reader.format(),
        Some(ImageFormat::Png)
            | Some(ImageFormat::Jpeg)
            | Some(ImageFormat::Gif)
            | Some(ImageFormat::WebP)
    ) {
        bail!("not a PNG, JPEG, GIF or WebP image");
    }
    // The dimensions are refused from the header, before the pixels are decoded.
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DECODE_EDGE);
    limits.max_image_height = Some(MAX_DECODE_EDGE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let image = reader.decode().context("decoding the image failed")?;
    // Small images keep their size: `thumbnail` would enlarge them.
    let scaled = if image.width() > MAX_EDGE || image.height() > MAX_EDGE {
        image.thumbnail(MAX_EDGE, MAX_EDGE)
    } else {
        image
    };
    Ok(Avatar { image: scaled })
}

impl Avatar {
    /// The bytes to send: PNG, or JPEG at a falling quality when PNG does not fit `limit`.
    pub fn encode(&self, limit: usize) -> Option<Vec<u8>> {
        if let Some(png) = encode_png(&self.image).filter(|bytes| bytes.len() <= limit) {
            return Some(png);
        }
        let rgb = DynamicImage::ImageRgb8(self.image.to_rgb8());
        for quality in JPEG_QUALITIES {
            let mut out = Cursor::new(Vec::new());
            if rgb
                .write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(
                    &mut out, quality,
                ))
                .is_ok()
                && out.get_ref().len() <= limit
            {
                return Some(out.into_inner());
            }
        }
        None
    }

    /// Sends the avatar as the bot's texture and waits `wait` for the server's answer. Every
    /// problem is a warning here, so an avatar failure never stops the run.
    pub async fn apply(&self, bot: &Bot, wait: Duration) {
        let limit = image_limit(bot);
        let Some(bytes) = self.encode(limit) else {
            tracing::warn!(
                limit,
                "avatar does not fit the server's image limit, connecting without it"
            );
            return;
        };
        if let Err(error) = bot.set_avatar(bytes) {
            tracing::warn!(%error, "sending the avatar failed");
            return;
        }
        let own = match bot.own_user() {
            Ok(user) => user.session,
            Err(error) => {
                tracing::warn!(%error, "the avatar was sent but not confirmed");
                return;
            }
        };
        // murmur does not acknowledge a texture, so a denial for the bot's own session inside the
        // delivery window is the only refusal that can be seen here.
        match tokio::time::timeout(
            wait,
            bot.wait_denied("the avatar", |d| d.session == Some(own)),
        )
        .await
        {
            Ok(Ok(denied)) => tracing::warn!(
                kind = ?denied.kind,
                reason = denied.reason.as_deref(),
                "the server refused the avatar, connecting without it"
            ),
            Ok(Err(error)) => tracing::warn!(%error, "the avatar was sent but not confirmed"),
            Err(_) => {}
        }
    }
}

/// The server's image limit in bytes, or murmur's default when it announces 0 or nothing.
fn image_limit(bot: &Bot) -> usize {
    match bot.state().server.image_message_length {
        Some(0) | None => DEFAULT_LIMIT,
        Some(bytes) => usize::try_from(bytes).unwrap_or(usize::MAX),
    }
}

fn encode_png(image: &DynamicImage) -> Option<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    image.write_to(&mut out, ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        encode_png(&DynamicImage::new_rgb8(width, height)).expect("encode")
    }

    /// A PNG that only declares `width` x `height` in its IHDR; enough for the size check.
    fn header(width: u32, height: u32) -> Vec<u8> {
        let body = {
            let mut body = Vec::from(b"IHDR");
            body.extend_from_slice(&width.to_be_bytes());
            body.extend_from_slice(&height.to_be_bytes());
            body.extend_from_slice(&[8, 6, 0, 0, 0]);
            body
        };
        let length = u32::try_from(body.len() - 4).expect("chunk length");
        let crc = crc32(&body);
        let mut file = Vec::from(b"\x89PNG\r\n\x1a\n");
        file.extend_from_slice(&length.to_be_bytes());
        file.extend_from_slice(&body);
        file.extend_from_slice(&crc.to_be_bytes());
        file
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xff_ff_ff_ff_u32;
        for byte in data {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xed_b8_83_20
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    #[test]
    fn decodes_by_content_and_scales_to_the_avatar_size() {
        let avatar = decode(&png(300, 200)).expect("decodes");
        assert_eq!((avatar.image.width(), avatar.image.height()), (128, 85));
        let avatar = decode(&png(60, 60)).expect("small images are not enlarged");
        assert_eq!((avatar.image.width(), avatar.image.height()), (60, 60));
    }

    #[test]
    fn refuses_other_content_and_decompression_bombs() {
        assert!(decode(b"not an image").is_err());
        // A file that only declares huge dimensions must be refused before the pixels.
        let error = decode(&header(MAX_DECODE_EDGE + 1, MAX_DECODE_EDGE + 1)).expect_err("refused");
        let by_size = |cause: &(dyn std::error::Error + 'static)| {
            matches!(
                cause.downcast_ref::<image::ImageError>(),
                Some(&image::ImageError::Limits(_))
            )
        };
        assert!(error.chain().any(by_size), "{error}");
        assert!(
            decode(&header(64, 64)).is_err(),
            "the header alone has no pixels"
        );
    }

    #[test]
    fn re_encodes_to_fit_the_server_limit() {
        // A noisy image: its PNG is larger than the limit below, so the JPEG path is taken.
        let mut buffer = image::RgbImage::new(128, 128);
        let mut state = 1u32;
        for pixel in buffer.pixels_mut() {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345_987);
            pixel[0] = state as u8;
            pixel[1] = (state >> 8) as u8;
            pixel[2] = (state >> 16) as u8;
        }
        let avatar = Avatar {
            image: DynamicImage::ImageRgb8(buffer),
        };
        assert!(
            encode_png(&avatar.image).expect("encode").len() > 20_000,
            "the test image needs a bigger PNG"
        );
        let fitted = avatar.encode(20_000).expect("fits with JPEG");
        assert!(fitted.len() <= 20_000);
        assert_eq!(fitted[..2], [0xff, 0xd8], "the fallback is a JPEG");
        assert!(
            avatar.encode(1000).is_none(),
            "nothing fits an absurd limit"
        );
    }

    #[test]
    fn one_source_and_the_environment_wins() {
        let config = AvatarConfig {
            avatar_url: Some("https://example.org/a.png".to_owned()),
            avatar_base64: Some("aGVsbG8=".to_owned()),
        };
        assert!(config.source_from(|_| None).is_err(), "both are set");
        // The file alone decides, and the environment beats it when it is set.
        let file = AvatarConfig {
            avatar_url: None,
            avatar_base64: Some("aGVsbG8=".to_owned()),
        };
        assert_eq!(
            file.source_from(|_| None).expect("the file"),
            Source::Base64("aGVsbG8=".to_owned())
        );
        assert_eq!(
            file.source_from(|name| (name == ENV_BASE64).then_some("env".to_owned()))
                .expect("the environment wins"),
            Source::Base64("env".to_owned())
        );
        assert!(
            file.source_from(
                |name| (name == ENV_URL).then_some("https://env.example.org/a.png".to_owned())
            )
            .is_err(),
            "one source from the environment and one from the file are still both"
        );
        assert_eq!(picked(ENV_URL, None, |_| None), None);
        assert_eq!(
            picked(ENV_URL, Some(""), |_| None),
            None,
            "an empty value counts as unset"
        );
        assert_eq!(
            AvatarConfig::default()
                .source_from(|_| None)
                .expect("unset"),
            Source::None
        );
    }

    #[test]
    fn decodes_base64() {
        assert_eq!(
            STANDARD.decode("aGVsbG8=").expect("decodes"),
            b"hello".to_vec()
        );
        assert!(STANDARD.decode("not base64!").is_err());
    }
}
