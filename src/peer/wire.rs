//! Plugin data messages of the peer module (data id `mumcord.peer`, spec 04 section 6.4) and the
//! Mumcord hello it listens for.
//!
//! The `mumcord.peer` messages themselves come from `mumcord-p2p`, the crate Mumcord uses too, so
//! the bot and the client cannot drift apart: compact JSON `{"v":1,"t":…}` of at most 1000 bytes
//! (the server's plugin data limit). Requests come from other clients and are untrusted; the
//! decoder checks the size before parsing, reads any `v >= 1` and ignores unknown fields. The
//! sender of a request is always the session the server stamped, never a payload field.

use serde::Deserialize;

pub use mumcord_p2p::{
    PEER_DATA_ID as DATA_ID, PeerMessage, RelayAnnouncement, RelayState, RelayToken, RequestId,
    TokenErrorReason,
};

/// Plugin data id of the hello every Mumcord client sends on connect and to people who join.
pub const HELLO_ID: &str = "mumcord.hello";
/// The hello feature of clients that can use P2P, and so the relay.
pub const P2P_FEATURE: &str = "p2p";

#[derive(Deserialize)]
struct Hello {
    app: String,
    v: u32,
    #[serde(default)]
    features: Vec<String>,
}

/// Whether the payload is a Mumcord hello (any version from 1 up), whatever it lists.
pub fn is_hello(data: &[u8]) -> bool {
    data.len() <= mumcord_protocol::PLUGIN_DATA_MAX
        && serde_json::from_slice::<Hello>(data).is_ok_and(|h| h.app == "mumcord" && h.v >= 1)
}

/// Whether the payload is a Mumcord hello (any version from 1 up) that lists `p2p`.
pub fn is_p2p_hello(data: &[u8]) -> bool {
    hello_lists(data, P2P_FEATURE)
}

/// Whether the payload is a Mumcord hello (any version from 1 up) that lists `feature`.
pub fn hello_lists(data: &[u8], feature: &str) -> bool {
    data.len() <= mumcord_protocol::PLUGIN_DATA_MAX
        && serde_json::from_slice::<Hello>(data).is_ok_and(|h| {
            h.app == "mumcord" && h.v >= 1 && h.features.iter().any(|f| f == feature)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RID: RequestId = RequestId(0x0123_4567_89ab_cdef);

    fn json(message: &PeerMessage) -> serde_json::Value {
        serde_json::from_slice(&message.encode().expect("encodes")).expect("json")
    }

    fn decode_request(data: &[u8]) -> Option<(RequestId, u32)> {
        match PeerMessage::decode(data) {
            Ok(PeerMessage::TokenRequest { rid, peer }) => Some((rid, peer)),
            _ => None,
        }
    }

    #[test]
    fn outgoing_messages_match_the_spec() {
        let relay = PeerMessage::Relay(RelayAnnouncement {
            host: "relay.example.org".into(),
            udp: 443,
            tcp: 443,
            fingerprint: [0xab; 32],
            state: RelayState::Up,
            seq: 4,
        });
        assert_eq!(
            json(&relay),
            serde_json::json!({"v":1,"t":"relay","host":"relay.example.org","udp":443,"tcp":443,
                "fp":"ab".repeat(32),"state":"up","seq":4})
        );
        let token = PeerMessage::Token {
            rid: RID,
            a: 3,
            b: 5,
            token: RelayToken::parse(&"t".repeat(43)).expect("token"),
            ttl: 120,
        };
        assert_eq!(
            json(&token),
            serde_json::json!({"v":1,"t":"token","rid":"0123456789abcdef","a":3,"b":5,
                "tok":"t".repeat(43),"ttl":120})
        );
        let reasons = [
            (TokenErrorReason::Denied, "denied"),
            (TokenErrorReason::PeerGone, "peer-gone"),
            (TokenErrorReason::PeerNotMumcord, "peer-not-mumcord"),
            (TokenErrorReason::RateLimited, "rate-limited"),
            (TokenErrorReason::RelayDown, "relay-down"),
            (TokenErrorReason::RelayFull, "relay-full"),
        ];
        for (reason, text) in reasons {
            assert_eq!(
                json(&PeerMessage::TokenError { rid: RID, reason }),
                serde_json::json!({"v":1,"t":"token-err","rid":"0123456789abcdef","r":text})
            );
        }
    }

    #[test]
    fn requests_are_read_leniently_and_everything_else_is_ignored() {
        assert_eq!(
            decode_request(br#"{"v":3,"t":"token-req","rid":"0123456789abcdef","peer":9,"x":1}"#),
            Some((RID, 9))
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
