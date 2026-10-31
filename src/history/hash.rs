//! Content hashes of Mumcord's history sync (mumcord QA.48). Ported from Mumcord's
//! `crates/engine/src/history_sync/hash.rs`: requesters recompute and compare them, so every
//! byte of the input must stay identical.
//!
//! A hash covers the channel, the sender (certificate hash, or name for users without one),
//! the send time rounded down to the second and the normalized body. Mumble has no server
//! timestamps, so every client stamps messages with its own receive time; two clients see the
//! same message a few milliseconds apart and may round to neighbouring seconds; requesters
//! compare with ±1 s, so a holder only sends the hash of its own stamp.
//! The body is reduced to its text (tags removed, common entities decoded, whitespace
//! collapsed), so different sanitizer versions on both sides still agree.

use ring::digest::{Context, SHA1_FOR_LEGACY_USE_ONLY};

use super::wire::{HASH_LEN, Hash};

/// Prefix that versions the hash input.
const DOMAIN: &[u8] = b"mumcord-history-v1";

/// What identifies a message for deduplication.
pub struct Content<'a> {
    pub channel: &'a str,
    pub sender_name: &'a str,
    pub sender_cert_hash: Option<&'a str>,
    pub sent_at: i64,
    pub body: &'a str,
}

/// The hash of a message.
pub fn content_hash(content: &Content<'_>) -> Hash {
    hash_at(
        content,
        &normalize(content.body),
        content.sent_at.div_euclid(1000),
    )
}

fn sender(content: &Content<'_>) -> String {
    match content.sender_cert_hash {
        Some(cert) => format!("cert:{cert}"),
        None => format!("name:{}", content.sender_name),
    }
}

fn hash_at(content: &Content<'_>, body: &str, second: i64) -> Hash {
    digest(&[
        DOMAIN,
        content.channel.as_bytes(),
        sender(content).as_bytes(),
        second.to_string().as_bytes(),
        body.as_bytes(),
    ])
}

fn digest(parts: &[&[u8]]) -> Hash {
    // SHA-1 is Mumcord's choice for a short dedup key, not a security property.
    let mut sha = Context::new(&SHA1_FOR_LEGACY_USE_ONLY);
    for part in parts {
        sha.update(part);
        sha.update(&[0]);
    }
    let digest = sha.finish();
    let mut hash = [0; HASH_LEN];
    hash.copy_from_slice(&digest.as_ref()[..HASH_LEN]);
    hash
}

/// The text of a message body: tags removed, common entities decoded, whitespace
/// collapsed. Bodies without text (e.g. only an image) use the whole markup instead.
pub fn normalize(body: &str) -> String {
    let mut text = String::with_capacity(body.len());
    let mut in_tag = false;
    for c in body.chars() {
        match c {
            '<' => {
                in_tag = true;
                text.push(' ');
            }
            '>' if in_tag => in_tag = false,
            _ if !in_tag => text.push(c),
            _ => {}
        }
    }
    let text = text
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    let collapsed = collapse(&text);
    if collapsed.is_empty() {
        collapse(body)
    } else {
        collapsed
    }
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(sent_at: i64, body: &str) -> Content<'_> {
        Content {
            channel: "Lobby",
            sender_name: "Alice",
            sender_cert_hash: Some("aa"),
            sent_at,
            body,
        }
    }

    #[test]
    fn markup_and_whitespace_do_not_change_the_hash() {
        let a = content_hash(&content(1_000, "<p>Hello   <b>world</b> &amp; you</p>"));
        let b = content_hash(&content(1_999, "Hello world & you"));
        assert_eq!(a, b, "same second, same text");
        assert_ne!(a, content_hash(&content(2_000, "Hello world & you")));
        assert_ne!(a, content_hash(&content(1_000, "Hello world")));
    }

    #[test]
    fn sender_and_channel_are_part_of_the_hash() {
        let base = content(1_000, "hi");
        let other_channel = Content {
            channel: "Raid",
            ..content(1_000, "hi")
        };
        let other_sender = Content {
            sender_cert_hash: Some("bb"),
            ..content(1_000, "hi")
        };
        let guest = Content {
            sender_cert_hash: None,
            ..content(1_000, "hi")
        };
        let h = content_hash(&base);
        assert_ne!(h, content_hash(&other_channel));
        assert_ne!(h, content_hash(&other_sender));
        assert_ne!(h, content_hash(&guest));
    }

    #[test]
    fn image_only_bodies_keep_their_markup() {
        assert_eq!(normalize("<img src=\"a.png\">"), "<img src=\"a.png\">");
        assert_ne!(
            content_hash(&content(1_000, "<img src=\"a.png\">")),
            content_hash(&content(1_000, "<img src=\"b.png\">"))
        );
    }

    fn hex(hash: Hash) -> String {
        hash.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Values computed with Mumcord's own `history_sync/hash.rs` (sha1 crate): the bot's hashes
    /// must match them byte for byte, or requesters drop every message.
    #[test]
    fn hashes_match_mumcord() {
        let cases = [
            (
                "Lobby",
                "Alice",
                Some("aa"),
                1_000,
                "<p>Hello   <b>world</b> &amp; you</p>",
                "8567ca7599e32538",
            ),
            (
                "Lobby",
                "Alice",
                Some("aa"),
                1_999,
                "Hello world & you",
                "8567ca7599e32538",
            ),
            ("Raid", "Alice", Some("aa"), 1_000, "hi", "40ffcee8eb4393d0"),
            (
                "Lobby",
                "Guest",
                None,
                1_700_000_000_123,
                "hi",
                "722587abd97cfb35",
            ),
            (
                "",
                "Bob",
                Some("0123456789abcdef0123456789abcdef01234567"),
                1_700_000_001_999,
                "<img src=\"a.png\">",
                "65d6c215f13fb4a7",
            ),
            (
                "Lobby/Sub",
                "Zoë",
                None,
                -1,
                "  multi\n line\ttext &lt;tag&gt; &quot;q&quot; &#39;s&#39; a&nbsp;b ",
                "41140ed4f64900ce",
            ),
        ];
        for (channel, sender_name, sender_cert_hash, sent_at, body, expected) in cases {
            let content = Content {
                channel,
                sender_name,
                sender_cert_hash,
                sent_at,
                body,
            };
            assert_eq!(hex(content_hash(&content)), expected, "{body:?}");
        }
        assert_eq!(
            normalize("  multi\n line\ttext &lt;tag&gt; &quot;q&quot; &#39;s&#39; a&nbsp;b "),
            "multi line text <tag> \"q\" 's' a b"
        );
    }
}
