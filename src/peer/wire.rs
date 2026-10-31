//! Plugin data messages of the peer module (data id `mumcord.peer`) and the Mumcord hello it
//! listens for.
//!
//! Every message is JSON `{"v":1,"t":…}` of at most 1000 bytes (the server's plugin data limit).
//! Requests come from other clients and are untrusted: the decoder caps the size, accepts higher
//! versions and ignores unknown types and extra fields. The sender of a request is always the
//! session the server stamped, never a payload field.

use serde::{Deserialize, Serialize};

/// Plugin data id of the peer messages.
pub const DATA_ID: &str = "mumcord.peer";
/// Plugin data id of the hello every Mumcord client sends on connect and to people who join.
pub const HELLO_ID: &str = "mumcord.hello";
/// The version this bot sends.
pub const VERSION: u32 = 1;
/// Hex characters of an epoch (8 random bytes).
pub const EPOCH_LEN: usize = 16;

/// Where the relay is: sent to a Mumcord client after its hello, and to all of them when the
/// epoch changes. A new epoch tells clients to drop what they knew about the relay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Relay {
    pub udp: Vec<String>,
    pub tcp: Vec<String>,
    /// Lowercase hex SHA-256 of the relay's certificate.
    pub fp: String,
    pub epoch: String,
}

/// A relay token for one pair of users, sent to both in one message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Token {
    /// The requester's request id.
    pub id: u32,
    /// Requester session.
    pub a: u32,
    /// Peer session.
    pub b: u32,
    pub token: String,
    /// Expiry in Unix seconds.
    pub exp: u64,
}

/// Why a token request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    /// The peer is not online, not a Mumcord client, or the requester itself.
    UnknownPeer,
    /// The requester or the peer has no client certificate.
    NoCert,
    RateLimited,
    /// The relay API did not accept the token.
    RelayUnavailable,
}

#[derive(Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Outgoing<'a> {
    Relay(&'a Relay),
    Token(&'a Token),
    Deny { id: u32, reason: DenyReason },
}

#[derive(Serialize)]
struct Envelope<'a> {
    v: u32,
    #[serde(flatten)]
    message: Outgoing<'a>,
}

fn encode(message: Outgoing<'_>) -> Vec<u8> {
    // Serializing plain structs of strings and numbers cannot fail.
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

pub fn encode_deny(id: u32, reason: DenyReason) -> Vec<u8> {
    encode(Outgoing::Deny { id, reason })
}

/// A token request: the sender wants to talk to the user with session `peer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenRequest {
    pub id: u32,
    pub peer: u32,
}

#[derive(Deserialize)]
struct Incoming {
    v: u32,
    t: String,
    id: Option<u32>,
    peer: Option<u32>,
}

/// Reads a `token_req`; anything else (other types, malformed, too large) is `None`.
pub fn decode_request(data: &[u8]) -> Option<TokenRequest> {
    if data.len() > mumcord_protocol::PLUGIN_DATA_MAX {
        return None;
    }
    let wire: Incoming = serde_json::from_slice(data).ok()?;
    if wire.v < 1 || wire.t != "token_req" {
        return None;
    }
    Some(TokenRequest {
        id: wire.id?,
        peer: wire.peer?,
    })
}

#[derive(Deserialize)]
struct Hello {
    app: String,
    v: u32,
}

/// Whether the payload is a Mumcord hello (any version from 1 up, features are not needed).
pub fn is_hello(data: &[u8]) -> bool {
    data.len() <= mumcord_protocol::PLUGIN_DATA_MAX
        && serde_json::from_slice::<Hello>(data).is_ok_and(|h| h.app == "mumcord" && h.v >= 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outgoing_messages_have_the_agreed_shape() {
        let relay = Relay {
            udp: vec!["r.example:3478".into()],
            tcp: vec!["r.example:443".into()],
            fp: "ab".repeat(32),
            epoch: "0123456789abcdef".into(),
        };
        let json: serde_json::Value = serde_json::from_slice(&encode_relay(&relay)).expect("json");
        assert_eq!(
            json,
            serde_json::json!({"v":1,"t":"relay","udp":["r.example:3478"],
                "tcp":["r.example:443"],"fp":"ab".repeat(32),"epoch":"0123456789abcdef"})
        );
        let token = Token {
            id: 7,
            a: 3,
            b: 5,
            token: "t".into(),
            exp: 99,
        };
        let json: serde_json::Value = serde_json::from_slice(&encode_token(&token)).expect("json");
        assert_eq!(
            json,
            serde_json::json!({"v":1,"t":"token","id":7,"a":3,"b":5,"token":"t","exp":99})
        );
        let json: serde_json::Value =
            serde_json::from_slice(&encode_deny(7, DenyReason::RelayUnavailable)).expect("json");
        assert_eq!(
            json,
            serde_json::json!({"v":1,"t":"deny","id":7,"reason":"relay_unavailable"})
        );
    }

    #[test]
    fn requests_are_read_leniently_and_everything_else_is_ignored() {
        assert_eq!(
            decode_request(br#"{"v":3,"t":"token_req","id":1,"peer":9,"x":true}"#),
            Some(TokenRequest { id: 1, peer: 9 })
        );
        for bad in [
            &br#"{"v":0,"t":"token_req","id":1,"peer":9}"#[..],
            br#"{"v":1,"t":"relay","id":1,"peer":9}"#,
            br#"{"v":1,"t":"token_req","peer":9}"#,
            br#"{"v":1,"t":"token_req","id":-1,"peer":9}"#,
            b"nope",
        ] {
            assert_eq!(decode_request(bad), None);
        }
        let mut big = br#"{"v":1,"t":"token_req","id":1,"peer":9,"pad":""#.to_vec();
        big.extend(std::iter::repeat_n(b'x', 1000));
        big.extend(br#""}"#);
        assert_eq!(decode_request(&big), None);
    }

    #[test]
    fn recognizes_the_mumcord_hello() {
        assert!(is_hello(
            br#"{"app":"mumcord","v":1,"features":["history-sync"]}"#
        ));
        assert!(is_hello(br#"{"app":"mumcord","v":4}"#));
        assert!(!is_hello(br#"{"app":"other","v":1}"#));
        assert!(!is_hello(br#"{"app":"mumcord","v":0}"#));
    }
}
