//! Canned message HTML (official-client-compatible) and a small avatar image for tests.

/// A 16x16 PNG (checkerboard), small enough for inline messages and avatars.
pub const PNG: &[u8] = include_bytes!("../assets/sample.png");

/// Ready-made messages for the chat rendering/sanitizer tests (mumcord AC-1.13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Sample {
    Bold,
    Link,
    /// Inline `data:` image.
    Image,
    /// Bold text, a link and an inline image in one message.
    Rich,
    /// Must be shown without executing the script.
    Script,
    /// Must be shown without loading the remote image.
    ExternalImage,
}

impl Sample {
    pub fn html(self) -> String {
        match self {
            Self::Bold => "<b>bold text</b> and normal text".to_owned(),
            Self::Link => r#"see <a href="https://www.mumble.info/">mumble.info</a>"#.to_owned(),
            Self::Image => image_tag(),
            Self::Rich => format!(
                r#"<b>bold text</b>, a link <a href="https://www.mumble.info/">mumble.info</a> and an image {}"#,
                image_tag()
            ),
            Self::Script => "before<script>alert('xss')</script>after".to_owned(),
            // `.invalid` never resolves, so a client that loads it anyway cannot leak anything.
            Self::ExternalImage => {
                r#"remote image: <img src="https://tracker.example.invalid/pixel.png">"#.to_owned()
            }
        }
    }
}

fn image_tag() -> String {
    format!(r#"<img src="data:image/png;base64,{}">"#, base64(PNG))
}

/// Standard base64 with padding (data URIs); tiny, so no extra dependency.
pub fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), expected);
        }
    }

    #[test]
    fn samples_contain_their_feature() {
        assert!(Sample::Rich.html().contains("<b>"));
        assert!(Sample::Rich.html().contains("<a href="));
        assert!(Sample::Rich.html().contains("data:image/png;base64,iVBOR"));
        assert!(Sample::Script.html().contains("<script>"));
        assert!(Sample::ExternalImage.html().contains("src=\"https://"));
    }
}
