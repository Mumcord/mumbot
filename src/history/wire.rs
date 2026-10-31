//! Wire format of Mumcord's history sync (mumcord FR-1.60, QA.48), sent as plugin data with the
//! data id [`DATA_ID`]. Ported from Mumcord's `crates/engine/src/history_sync/wire.rs`; the bytes
//! must stay identical, so keep both in step.
//!
//! A packet is a compact JSON header, optionally followed by `\n` and a binary body:
//! `{"v":1,"t":"part","r":7,"i":0,"last":false}\n<bytes>`. Header kinds:
//!
//! | `t`      | Body                         | Meaning                                              |
//! |----------|------------------------------|------------------------------------------------------|
//! | `areq`   | –                            | Who holds channel `c` in `[from, to)`? (to everyone) |
//! | `avail`  | –                            | Holder's summary: `n` messages from `old` to `new`   |
//! | `hreq`   | –                            | Send me the hashes of your messages in `[from, to)`  |
//! | `hashes` | 8-byte content hashes        | Chunk `i` of the hash list (oldest first)            |
//! | `get`    | bitmap (bit k = hash k)      | Send these messages from the hash list               |
//! | `part`   | slice of the message stream  | Chunk `i` of the JSON message array                  |
//! | `deny`   | –                            | The holder does not answer request `r`               |
//!
//! Every packet fits the server's plugin data limit ([`PLUGIN_DATA_MAX`]); longer data is
//! chunked. Everything received is untrusted: the decoder checks sizes and shapes, the
//! session logic checks that it answers an own request.

use mumcord_protocol::PLUGIN_DATA_MAX;
use serde::{Deserialize, Serialize};

/// Plugin data id of history sync packets; other ids are ignored.
pub const DATA_ID: &str = "mumcord.history";
/// The packet version this client sends. Any version >= 1 is accepted on receipt and unknown
/// fields are ignored. Minor additions keep v=1 and old clients ignore unknown fields; a breaking change gets a new data id.
pub const VERSION: u32 = 1;
/// Length of a content hash on the wire.
pub const HASH_LEN: usize = 8;
/// Most messages one request covers (FR-1.60).
pub const MAX_MESSAGES: usize = 500;
/// Body bytes per packet; leaves room for the largest header.
pub const BODY_BUDGET: usize = PLUGIN_DATA_MAX - 100;
/// Hashes per `hashes` chunk.
pub const HASHES_PER_CHUNK: usize = BODY_BUDGET / HASH_LEN;

/// A truncated content hash (see [`super::hash`]).
pub type Hash = [u8; HASH_LEN];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    AvailabilityRequest {
        req: u32,
        channel: u32,
        from: i64,
        to: i64,
    },
    Availability {
        req: u32,
        channel: u32,
        count: u32,
        oldest: i64,
        newest: i64,
    },
    HashRequest {
        req: u32,
        channel: u32,
        from: i64,
        to: i64,
    },
    Hashes {
        req: u32,
        index: u16,
        last: bool,
        hashes: Vec<Hash>,
    },
    Fetch {
        req: u32,
        wanted: Vec<u8>,
    },
    Part {
        req: u32,
        index: u16,
        last: bool,
        data: Vec<u8>,
    },
    Deny {
        req: u32,
    },
}

/// One message in the stream of `part` packets (a JSON array of these).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireMessage {
    /// Send time at the holder (Unix milliseconds).
    pub ts: i64,
    /// Sender name.
    pub n: String,
    /// Sender certificate hash, if the sender had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h: Option<String>,
    /// Body as stored by the holder (sanitized HTML; sanitized again on receipt).
    pub b: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum WireError {
    TooLarge,
    Malformed,
    Version(u32),
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => f.write_str("packet too large"),
            Self::Malformed => f.write_str("packet is malformed"),
            Self::Version(v) => write!(f, "unknown packet version {v}"),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Header {
    v: u32,
    #[serde(flatten)]
    kind: Kind,
}

#[derive(Deserialize)]
struct VersionOnly {
    v: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Kind {
    Areq {
        r: u32,
        c: u32,
        from: i64,
        to: i64,
    },
    Avail {
        r: u32,
        c: u32,
        n: u32,
        old: i64,
        new: i64,
    },
    Hreq {
        r: u32,
        c: u32,
        from: i64,
        to: i64,
    },
    Hashes {
        r: u32,
        i: u16,
        last: bool,
    },
    Get {
        r: u32,
    },
    Part {
        r: u32,
        i: u16,
        last: bool,
    },
    Deny {
        r: u32,
    },
}

/// The bytes of `packet`. Packets built by [`chunk_hashes`], [`chunk_stream`] and with a
/// bitmap for at most [`MAX_MESSAGES`] hashes always fit [`PLUGIN_DATA_MAX`].
pub fn encode(packet: &Packet) -> Vec<u8> {
    let (kind, body): (Kind, Vec<u8>) = match packet {
        Packet::AvailabilityRequest {
            req,
            channel,
            from,
            to,
        } => (
            Kind::Areq {
                r: *req,
                c: *channel,
                from: *from,
                to: *to,
            },
            Vec::new(),
        ),
        Packet::Availability {
            req,
            channel,
            count,
            oldest,
            newest,
        } => (
            Kind::Avail {
                r: *req,
                c: *channel,
                n: *count,
                old: *oldest,
                new: *newest,
            },
            Vec::new(),
        ),
        Packet::HashRequest {
            req,
            channel,
            from,
            to,
        } => (
            Kind::Hreq {
                r: *req,
                c: *channel,
                from: *from,
                to: *to,
            },
            Vec::new(),
        ),
        Packet::Hashes {
            req,
            index,
            last,
            hashes,
        } => (
            Kind::Hashes {
                r: *req,
                i: *index,
                last: *last,
            },
            hashes.concat(),
        ),
        Packet::Fetch { req, wanted } => (Kind::Get { r: *req }, wanted.clone()),
        Packet::Part {
            req,
            index,
            last,
            data,
        } => (
            Kind::Part {
                r: *req,
                i: *index,
                last: *last,
            },
            data.clone(),
        ),
        Packet::Deny { req } => (Kind::Deny { r: *req }, Vec::new()),
    };
    // Serializing numbers and booleans cannot fail.
    let mut out = serde_json::to_vec(&Header { v: VERSION, kind }).unwrap_or_default();
    if !body.is_empty() {
        out.push(b'\n');
        out.extend_from_slice(&body);
    }
    out
}

/// Parses a received packet.
pub fn decode(data: &[u8]) -> Result<Packet, WireError> {
    if data.len() > PLUGIN_DATA_MAX {
        return Err(WireError::TooLarge);
    }
    let (head, body) = match data.iter().position(|&b| b == b'\n') {
        Some(at) => (&data[..at], &data[at + 1..]),
        None => (data, &data[data.len()..]),
    };
    let version: VersionOnly = serde_json::from_slice(head).map_err(|_| WireError::Malformed)?;
    if version.v < VERSION {
        return Err(WireError::Version(version.v));
    }
    let header: Header = serde_json::from_slice(head).map_err(|_| WireError::Malformed)?;
    let no_body = |packet: Packet| {
        if body.is_empty() {
            Ok(packet)
        } else {
            Err(WireError::Malformed)
        }
    };
    match header.kind {
        Kind::Areq { r, c, from, to } => no_body(Packet::AvailabilityRequest {
            req: r,
            channel: c,
            from,
            to,
        }),
        Kind::Avail { r, c, n, old, new } => no_body(Packet::Availability {
            req: r,
            channel: c,
            count: n,
            oldest: old,
            newest: new,
        }),
        Kind::Hreq { r, c, from, to } => no_body(Packet::HashRequest {
            req: r,
            channel: c,
            from,
            to,
        }),
        Kind::Hashes { r, i, last } => {
            if body.len() % HASH_LEN != 0 {
                return Err(WireError::Malformed);
            }
            let hashes = body.as_chunks::<HASH_LEN>().0.to_vec();
            Ok(Packet::Hashes {
                req: r,
                index: i,
                last,
                hashes,
            })
        }
        Kind::Get { r } => Ok(Packet::Fetch {
            req: r,
            wanted: body.to_vec(),
        }),
        Kind::Part { r, i, last } => Ok(Packet::Part {
            req: r,
            index: i,
            last,
            data: body.to_vec(),
        }),
        Kind::Deny { r } => no_body(Packet::Deny { req: r }),
    }
}

/// The `hashes` packets for a hash list; an empty list is one empty, final chunk.
pub fn chunk_hashes(req: u32, hashes: &[Hash]) -> Vec<Packet> {
    if hashes.is_empty() {
        return vec![Packet::Hashes {
            req,
            index: 0,
            last: true,
            hashes: Vec::new(),
        }];
    }
    let count = hashes.len().div_ceil(HASHES_PER_CHUNK);
    hashes
        .chunks(HASHES_PER_CHUNK)
        .enumerate()
        .map(|(i, chunk)| Packet::Hashes {
            req,
            index: u16::try_from(i).unwrap_or(u16::MAX),
            last: i + 1 == count,
            hashes: chunk.to_vec(),
        })
        .collect()
}

/// The `part` packets for a message stream.
pub fn chunk_stream(req: u32, stream: &[u8]) -> Vec<Packet> {
    if stream.is_empty() {
        return vec![Packet::Part {
            req,
            index: 0,
            last: true,
            data: Vec::new(),
        }];
    }
    let count = stream.len().div_ceil(BODY_BUDGET);
    stream
        .chunks(BODY_BUDGET)
        .enumerate()
        .map(|(i, chunk)| Packet::Part {
            req,
            index: u16::try_from(i).unwrap_or(u16::MAX),
            last: i + 1 == count,
            data: chunk.to_vec(),
        })
        .collect()
}

/// A bitmap with bit `k` set for every index in `selected` (`len` hashes in the list).
pub fn bitmap(len: usize, selected: impl IntoIterator<Item = usize>) -> Vec<u8> {
    let mut bits = vec![0u8; len.div_ceil(8)];
    for k in selected {
        if k < len {
            bits[k / 8] |= 1 << (k % 8);
        }
    }
    bits
}

/// Indexes set in `bits`, below `len`.
pub fn selected(bits: &[u8], len: usize) -> Vec<usize> {
    (0..len)
        .filter(|&k| bits.get(k / 8).is_some_and(|b| b & (1 << (k % 8)) != 0))
        .collect()
}

pub fn encode_messages(messages: &[WireMessage]) -> Vec<u8> {
    // Serializing strings and numbers cannot fail.
    serde_json::to_vec(messages).unwrap_or_default()
}

pub fn decode_messages(stream: &[u8]) -> Result<Vec<WireMessage>, WireError> {
    serde_json::from_slice(stream).map_err(|_| WireError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(packet: Packet) {
        let data = encode(&packet);
        assert!(data.len() <= PLUGIN_DATA_MAX, "{} bytes", data.len());
        assert_eq!(decode(&data), Ok(packet));
    }

    #[test]
    fn every_kind_roundtrips() {
        roundtrip(Packet::AvailabilityRequest {
            req: u32::MAX,
            channel: 3,
            from: 1_700_000_000_000,
            to: 1_700_000_600_000,
        });
        roundtrip(Packet::Availability {
            req: 1,
            channel: 3,
            count: 500,
            oldest: 1,
            newest: 2,
        });
        roundtrip(Packet::HashRequest {
            req: 2,
            channel: 0,
            from: -5,
            to: i64::MAX,
        });
        roundtrip(Packet::Fetch {
            req: 3,
            wanted: bitmap(MAX_MESSAGES, 0..MAX_MESSAGES),
        });
        roundtrip(Packet::Deny { req: 4 });
    }

    #[test]
    fn hash_lists_and_streams_are_chunked_under_the_server_limit() {
        let hashes: Vec<Hash> = (0..MAX_MESSAGES)
            .map(|i| [u8::try_from(i % 256).unwrap_or(0); HASH_LEN])
            .collect();
        let chunks = chunk_hashes(u32::MAX, &hashes);
        assert_eq!(chunks.len(), MAX_MESSAGES.div_ceil(HASHES_PER_CHUNK));
        let mut back = Vec::new();
        for (i, chunk) in chunks.iter().enumerate() {
            let data = encode(chunk);
            assert!(data.len() <= PLUGIN_DATA_MAX, "{} bytes", data.len());
            let Ok(Packet::Hashes {
                index,
                last,
                hashes,
                ..
            }) = decode(&data)
            else {
                panic!("hashes chunk");
            };
            assert_eq!(usize::from(index), i);
            assert_eq!(last, i + 1 == chunks.len());
            back.extend(hashes);
        }
        assert_eq!(back, hashes);

        // A stream full of newlines and quotes survives (the body is raw bytes).
        let stream = b"[\"\n\"]".repeat(2000);
        let parts = chunk_stream(u32::MAX, &stream);
        let mut joined = Vec::new();
        for part in &parts {
            let data = encode(part);
            assert!(data.len() <= PLUGIN_DATA_MAX, "{} bytes", data.len());
            let Ok(Packet::Part { data, .. }) = decode(&data) else {
                panic!("part");
            };
            joined.extend(data);
        }
        assert_eq!(joined, stream);
        assert_eq!(chunk_stream(1, &[]).len(), 1);
        assert_eq!(chunk_hashes(1, &[]).len(), 1);
    }

    #[test]
    fn bytes_match_mumcord() {
        // The request Mumcord's history_sync tests send; a holder must read exactly this.
        let request = br#"{"v":1,"t":"hreq","r":77,"c":1,"from":1699999997000,"to":1700000001000}"#;
        let packet = Packet::HashRequest {
            req: 77,
            channel: 1,
            from: 1_699_999_997_000,
            to: 1_700_000_001_000,
        };
        assert_eq!(decode(request), Ok(packet.clone()));
        assert_eq!(encode(&packet), request);
        assert_eq!(
            encode(&Packet::Availability {
                req: 1,
                channel: 2,
                count: 3,
                oldest: 4,
                newest: 5
            }),
            br#"{"v":1,"t":"avail","r":1,"c":2,"n":3,"old":4,"new":5}"#
        );
        assert_eq!(
            encode(&Packet::Hashes {
                req: 1,
                index: 0,
                last: true,
                hashes: vec![[0xab; HASH_LEN]]
            }),
            [
                &br#"{"v":1,"t":"hashes","r":1,"i":0,"last":true}"#[..],
                b"\n",
                &[0xab; HASH_LEN]
            ]
            .concat()
        );
    }

    #[test]
    fn newer_version_with_extra_fields_decodes() {
        assert_eq!(
            decode(br#"{"v":2,"t":"deny","r":1,"future":true}"#),
            Ok(Packet::Deny { req: 1 })
        );
    }

    #[test]
    fn untrusted_packets_are_rejected() {
        assert_eq!(
            decode(&vec![b' '; PLUGIN_DATA_MAX + 1]),
            Err(WireError::TooLarge)
        );
        assert_eq!(decode(b"not json"), Err(WireError::Malformed));
        assert_eq!(decode(br#"{"v":0,"t":"new"}"#), Err(WireError::Version(0)));
        assert_eq!(
            decode(br#"{"v":1,"t":"bogus","r":1}"#),
            Err(WireError::Malformed)
        );
        assert_eq!(
            decode(br#"{"v":1,"t":"deny","r":1}"#),
            Ok(Packet::Deny { req: 1 })
        );
        assert_eq!(
            decode(b"{\"v\":1,\"t\":\"deny\",\"r\":1}\nextra"),
            Err(WireError::Malformed),
            "kinds without a body must not carry one"
        );
        assert_eq!(
            decode(b"{\"v\":1,\"t\":\"hashes\",\"r\":1,\"i\":0,\"last\":true}\n12345"),
            Err(WireError::Malformed),
            "hash lists come in whole hashes"
        );
        assert_eq!(decode_messages(b"[{\"ts\":1}]"), Err(WireError::Malformed));
    }

    #[test]
    fn bitmaps_select_indexes() {
        let bits = bitmap(10, [0, 3, 9, 42]);
        assert_eq!(bits.len(), 2);
        assert_eq!(selected(&bits, 10), [0, 3, 9]);
        assert_eq!(selected(&bits, 4), [0, 3], "bits past the list are ignored");
        assert!(selected(&[], 10).is_empty());
    }

    #[test]
    fn messages_roundtrip() {
        let messages = vec![
            WireMessage {
                ts: 1,
                n: "Alice".into(),
                h: Some("ab".into()),
                b: "<b>hi</b>\n".into(),
            },
            WireMessage {
                ts: 2,
                n: "Guest".into(),
                h: None,
                b: String::new(),
            },
        ];
        assert_eq!(decode_messages(&encode_messages(&messages)), Ok(messages));
    }
}
