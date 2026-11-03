//! The password proof for locked channels (Mumcord issue #382), ported from Mumcord's
//! `crates/engine/src/history_sync/proof.rs`; the bytes must stay identical.
//!
//! A holder with channel passwords (access tokens) shares a locked channel's history only with
//! requesters who prove that they know one of its tokens, without the token ever leaving either
//! side: the holder sends a fresh random nonce, the requester answers with one HMAC per own
//! token over the nonce, the channel and both certificate hashes, and the holder recomputes them
//! with its own tokens. Wire contract: Mumcord's `docs/history-sync.md`.

use ring::hmac;
use ring::rand::{SecureRandom, SystemRandom};

/// Length of a challenge nonce (128 bits).
pub const NONCE_LEN: usize = 16;
/// Length of one proof (HMAC-SHA256).
pub const MAC_LEN: usize = 32;
/// Most proofs in one answer: one per access token the requester holds.
pub const MAX_PROOFS: usize = 16;
/// A challenge older than this is not answered any more.
pub const NONCE_TTL_MS: i64 = 10_000;
/// Domain label of the key derivation, so a proof key is never a plain hash of the token.
const KDF_LABEL: &[u8] = b"mumcord.history.token.v1";

pub type Nonce = [u8; NONCE_LEN];
pub type Mac = [u8; MAC_LEN];

/// A fresh random nonce; `None` when the system has no randomness to give (no challenge then).
pub fn nonce() -> Option<Nonce> {
    let mut nonce = [0; NONCE_LEN];
    SystemRandom::new().fill(&mut nonce).ok()?;
    Some(nonce)
}

/// `KDF(token)`: HMAC-SHA256 keyed with [`KDF_LABEL`] over the token's UTF-8 bytes.
fn token_key(token: &str) -> hmac::Key {
    let derived = hmac::sign(
        &hmac::Key::new(hmac::HMAC_SHA256, KDF_LABEL),
        token.as_bytes(),
    );
    hmac::Key::new(hmac::HMAC_SHA256, derived.as_ref())
}

/// The signed input: nonce, channel id (u32 big endian), then both certificate hashes, each
/// prefixed with its length in bytes (u8), so no field can be shifted into another.
fn input(nonce: &Nonce, channel: u32, requester_cert: &str, answerer_cert: &str) -> Vec<u8> {
    let mut data = Vec::with_capacity(NONCE_LEN + 4 + 2 + 2 * 40);
    data.extend_from_slice(nonce);
    data.extend_from_slice(&channel.to_be_bytes());
    for cert in [requester_cert, answerer_cert] {
        let bytes = cert.as_bytes();
        let len = u8::try_from(bytes.len()).unwrap_or(u8::MAX);
        data.push(len);
        data.extend_from_slice(&bytes[..usize::from(len)]);
    }
    data
}

/// The requester's proofs: one per token, at most [`MAX_PROOFS`]. The bot never requests; this
/// is the Mumcord side, used to test the bot.
pub fn prove(
    tokens: &[String],
    nonce: &Nonce,
    channel: u32,
    requester_cert: &str,
    answerer_cert: &str,
) -> Vec<Mac> {
    let data = input(nonce, channel, requester_cert, answerer_cert);
    tokens
        .iter()
        .take(MAX_PROOFS)
        .filter_map(|token| {
            hmac::sign(&token_key(token), &data)
                .as_ref()
                .try_into()
                .ok()
        })
        .collect()
}

/// Whether any proof matches any of the holder's own tokens (compared in constant time).
pub fn verify(
    tokens: &[String],
    proofs: &[Mac],
    nonce: &Nonce,
    channel: u32,
    requester_cert: &str,
    answerer_cert: &str,
) -> bool {
    let data = input(nonce, channel, requester_cert, answerer_cert);
    tokens.iter().any(|token| {
        let key = token_key(token);
        proofs
            .iter()
            .take(MAX_PROOFS)
            .any(|mac| hmac::verify(&key, &data, mac).is_ok())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn tokens(list: &[&str]) -> Vec<String> {
        list.iter().map(|t| (*t).to_owned()).collect()
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The shared test vector of `docs/history.md`, computed independently from the wire
    /// contract (Python `hmac`), so the bot and Mumcord derive the same proof.
    #[test]
    fn the_shared_test_vector_matches() {
        let nonce: Nonce = std::array::from_fn(|i| u8::try_from(i).unwrap_or(0));
        let proof = prove(&tokens(&["s3cret"]), &nonce, 7, A, B);
        assert_eq!(
            hex(&proof[0]),
            "89745f2f4b58fa89eb78ba68d735b8016f90b438fc15d1effcc2ab7d1cd95297"
        );
    }

    #[test]
    fn the_right_token_proves_and_anything_else_does_not() {
        let nonce = nonce().expect("randomness");
        let proofs = prove(&tokens(&["other", "secret"]), &nonce, 7, A, B);
        assert_eq!(proofs.len(), 2);
        assert!(verify(&tokens(&["secret"]), &proofs, &nonce, 7, A, B));
        assert!(!verify(&tokens(&["wrong"]), &proofs, &nonce, 7, A, B));
        assert!(!verify(&[], &proofs, &nonce, 7, A, B));
        // Bound to the nonce, the channel and both certificates.
        let mut other = nonce;
        other[0] ^= 1;
        assert!(!verify(&tokens(&["secret"]), &proofs, &other, 7, A, B));
        assert!(!verify(&tokens(&["secret"]), &proofs, &nonce, 8, A, B));
        assert!(!verify(&tokens(&["secret"]), &proofs, &nonce, 7, B, A));
    }

    #[test]
    fn proofs_never_contain_the_token_or_its_plain_hash() {
        let nonce = [0; NONCE_LEN];
        let proof = prove(&tokens(&["secret"]), &nonce, 1, A, B)[0];
        let plain = ring::digest::digest(&ring::digest::SHA256, b"secret");
        assert_ne!(proof.as_slice(), plain.as_ref());
        assert!(!proof.windows(6).any(|w| w == b"secret"));
    }

    #[test]
    fn at_most_sixteen_proofs_are_made_and_checked() {
        let many: Vec<String> = (0..20).map(|i| format!("t{i}")).collect();
        let nonce = [1; NONCE_LEN];
        let proofs = prove(&many, &nonce, 1, A, B);
        assert_eq!(proofs.len(), MAX_PROOFS);
        assert!(!verify(&tokens(&["t19"]), &proofs, &nonce, 1, A, B));
    }

    #[test]
    fn nonces_are_random() {
        assert_ne!(nonce(), nonce());
    }
}
