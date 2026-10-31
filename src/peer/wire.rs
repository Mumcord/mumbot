//! Plugin data messages of the peer module (data id `mumcord.peer`, spec 04 section 6.4) and the
//! Mumcord hello it listens for.
//!
//! Every message is compact JSON `{"v":1,"t":…}` of at most 1000 bytes (the server's plugin data
//! limit). Requests come from other clients and are untrusted: the size is checked before
//! parsing, any `v >= 1` is read, unknown fields and types are ignored. The sender of a request is
//! always the session the server stamped, never a payload field.

use serde::{Deserialize, Serialize};

/// Plugin data id of the peer messages.
pub const DATA_ID: &str = "mumcord.peer";
/// Plugin data id of the hello every Mumcord client sends on connect and to people who join.
pub const HELLO_ID: &str = "mumcord.hello";
/// The hello feature of clients that can use P2P, and so the relay.
pub const P2P_FEATURE: &str = "p2p";
/// The version this bot sends.
pub const VERSION: u32 = 1;

/// Whether the relay is usable, as far as the bot can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Up,
    Down,
}

/// The relay announcement: to one client after its hello, or to all on a change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Relay {
    pub host: String,
    pub udp: u16,
    pub tcp: u16,
    /// Lowercase hex SHA-256 of the relay certificate DER.
    pub fp: String,
    pub state: State,
    /// Grows with every change; clients ignore an older one.
    pub seq: u32,
}

/// A relay token for one pair, sent to both in one message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Token {
    /// The requester's request id.
    pub rid: String,
    /// Requester session.
    pub a: u32,
    /// Peer session.
    pub b: u32,
    pub tok: String,
    /// Seconds the token stays valid.
    pub ttl: u16,
}

/// Why a token request failed (`r` of `token-err`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TokenError {
    /// The requester may not have a token: no certificate, no P2P hello, or itself as peer.
    Denied,
    PeerGone,
    PeerNotMumcord,
    RateLimited,
    RelayDown,
    RelayFull,
}

#[derive(Serialize)]
#[serde(tag = "t", rename_all = "kebab-case")]
enum Outgoing<'a> {
    Relay(&'a Relay),
    Token(&'a Token),
    TokenErr { rid: &'a str, r: TokenError },
}

#[derive(Serialize)]
struct Envelope<'a> {
    v: u32,
    #[serde(flatten)]
    message: Outgoing<'a>,
}

fn encode(message: Outgoing<'_>) -> Vec<u8> {
    // Plain structs of strings and numbers always serialize.
    serde_json::to_vec(&Envelope {
        v: VERSION,
        message,
    })
    .unwrap_or_default()
}

pub fn encode_relay(relay: &Relay) -> Vec<u8> {
    encode(Outgoing::Relay(relay))
}

pub fn encode_token(token: &Token) -> Vec<u8> {
    encode(Outgoing::Token(token))
}

pub fn encode_token_err(rid: &str, error: TokenError) -> Vec<u8> {
    encode(Outgoing::TokenErr { rid, r: error })
}

/// A `token-req`: the sender wants a relay token for itself and session `peer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRequest {
    pub rid: String,
    pub peer: u32,
}

#[derive(Deserialize)]
struct Incoming {
    v: u32,
    t: String,
    rid: Option<String>,
    peer: Option<u32>,
}

/// Reads a `token-req`. Other types, malformed or oversized data and a `rid` that is not 16
/// lowercase hex digits give `None`; without a valid `rid` there is nothing to answer to.
pub fn decode_request(data: &[u8]) -> Option<TokenRequest> {
    if data.len() > mumcord_protocol::PLUGIN_DATA_MAX {
        return None;
    }
    let wire: Incoming = serde_json::from_slice(data).ok()?;
    if wire.v < 1 || wire.t != "token-req" {
        return None;
    }
    let rid = wire.rid?;
    let valid = rid.len() == 16 && rid.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    valid.then_some(TokenRequest {
        rid,
        peer: wire.peer?,
    })
}

#[derive(Deserialize)]
struct Hello {
    app: String,
    v: u32,
    #[serde(default)]
    features: Vec<String>,
}

/// Whether the payload is a Mumcord hello (any version from 1 up) that lists `p2p`.
pub fn is_p2p_hello(data: &[u8]) -> bool {
    data.len() <= mumcord_protocol::PLUGIN_DATA_MAX
        && serde_json::from_slice::<Hello>(data).is_ok_and(|h| {
            h.app == "mumcord" && h.v >= 1 && h.features.iter().any(|f| f == P2P_FEATURE)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(data: &[u8]) -> serde_json::Value {
        serde_json::from_slice(data).expect("json")
    }

    #[test]
    fn outgoing_messages_match_the_spec() {
        let relay = Relay {
            host: "relay.example.org".into(),
            udp: 443,
            tcp: 443,
            fp: "ab".repeat(32),
            state: State::Up,
            seq: 4,
        };
        assert_eq!(
            json(&encode_relay(&relay)),
            serde_json::json!({"v":1,"t":"relay","host":"relay.example.org","udp":443,"tcp":443,
                "fp":"ab".repeat(32),"state":"up","seq":4})
        );
        let token = Token {
            rid: "0123456789abcdef".into(),
            a: 3,
            b: 5,
            tok: "t".repeat(43),
            ttl: 120,
        };
        assert_eq!(
            json(&encode_token(&token)),
            serde_json::json!({"v":1,"t":"token","rid":"0123456789abcdef","a":3,"b":5,
                "tok":"t".repeat(43),"ttl":120})
        );
        let reasons = [
            (TokenError::Denied, "denied"),
            (TokenError::PeerGone, "peer-gone"),
            (TokenError::PeerNotMumcord, "peer-not-mumcord"),
            (TokenError::RateLimited, "rate-limited"),
            (TokenError::RelayDown, "relay-down"),
            (TokenError::RelayFull, "relay-full"),
        ];
        for (error, text) in reasons {
            assert_eq!(
                json(&encode_token_err("0123456789abcdef", error)),
                serde_json::json!({"v":1,"t":"token-err","rid":"0123456789abcdef","r":text})
            );
        }
    }

    #[test]
    fn requests_are_read_leniently_and_everything_else_is_ignored() {
        assert_eq!(
            decode_request(br#"{"v":3,"t":"token-req","rid":"0123456789abcdef","peer":9,"x":1}"#),
            Some(TokenRequest {
                rid: "0123456789abcdef".into(),
                peer: 9
            })
        );
        for bad in [
            &br#"{"v":0,"t":"token-req","rid":"0123456789abcdef","peer":9}"#[..],
            br#"{"v":1,"t":"relay","rid":"0123456789abcdef","peer":9}"#,
            br#"{"v":1,"t":"token-req","peer":9}"#,
            br#"{"v":1,"t":"token-req","rid":"0123456789ABCDEF","peer":9}"#,
            br#"{"v":1,"t":"token-req","rid":"0123","peer":9}"#,
            br#"{"v":1,"t":"token-req","rid":"0123456789abcdef","peer":-1}"#,
            b"nope",
        ] {
            assert_eq!(decode_request(bad), None);
        }
        let mut big =
            br#"{"v":1,"t":"token-req","rid":"0123456789abcdef","peer":9,"pad":""#.to_vec();
        big.extend(std::iter::repeat_n(b'x', 1000));
        big.extend(br#""}"#);
        assert_eq!(decode_request(&big), None);
    }

    #[test]
    fn only_a_mumcord_hello_with_p2p_counts() {
        assert!(is_p2p_hello(
            br#"{"app":"mumcord","v":1,"features":["history-sync","p2p"]}"#
        ));
        assert!(is_p2p_hello(
            br#"{"app":"mumcord","v":4,"features":["p2p"],"x":1}"#
        ));
        assert!(!is_p2p_hello(
            br#"{"app":"mumcord","v":1,"features":["history-sync"]}"#
        ));
        assert!(!is_p2p_hello(br#"{"app":"mumcord","v":1}"#));
        assert!(!is_p2p_hello(
            br#"{"app":"other","v":1,"features":["p2p"]}"#
        ));
        assert!(!is_p2p_hello(
            br#"{"app":"mumcord","v":0,"features":["p2p"]}"#
        ));
    }
}
