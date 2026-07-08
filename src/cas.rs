//! Content-addressable storage (CAS) hashing for rosary.
//!
//! BLAKE3, lowercase-hex over the 256-bit digest (64 chars) — the ecosystem
//! CAS wire format shared with **cloister** (`cas-hash`: `blake3Hash` /
//! `blake3Hex` / `BLAKE3_DIGEST_LEN`) and **mache** (OCI blob digests via
//! `zeebo/blake3`, v1 wire digest). Same algorithm + encoding ⇒ a blob or
//! observation hashed in rosary has the SAME digest as in cloister/mache,
//! which is the whole point of a shared CAS (rosary-a3ab19).
//!
//! NOT for: DSSE/in-toto attestation digests (`dsse.rs`) or provider webhook
//! HMACs (`serve/webhook.rs`) — those are sha256 by external spec, a
//! different interop boundary, deliberately left as sha256.

/// Content hash of arbitrary bytes: lowercase hex of the BLAKE3-256 digest
/// (64 hex chars). The canonical rosary CAS primitive.
#[allow(dead_code)] // API surface — CAS primitive; observation payload_hash + future blob CAS
pub fn content_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cross-impl interop guard: rosary's CAS digest MUST equal the published
    /// BLAKE3 test vectors, so blobs are addressable identically across the
    /// ecosystem (cloister/mache). If this fails, rosary's CAS has silently
    /// diverged from the standard algorithm.
    #[test]
    fn content_hash_matches_blake3_known_vectors() {
        assert_eq!(
            content_hash(b""),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
        assert_eq!(
            content_hash(b"abc"),
            "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
        );
    }

    #[test]
    fn content_hash_is_deterministic_64_hex() {
        let h = content_hash(b"rosary");
        assert_eq!(h, content_hash(b"rosary"), "deterministic");
        assert_ne!(h, content_hash(b"rosary2"), "input-sensitive");
        assert_eq!(h.len(), 64, "blake3-256 hex = 64 chars");
        assert!(h.bytes().all(|b| b.is_ascii_hexdigit()), "lowercase hex");
    }

    /// Golden-vector proof (rosary-bf6c74): rosary's `content_hash` is
    /// byte-for-byte identical to LLO's canonical `ContentAddressed::hash`
    /// across a spread of inputs — so swapping `content_hash` onto the LLO
    /// primitive (rosary-bf8121) is a pure refactor, and rosary can't drift off
    /// the substrate's BLAKE3 lock (the exact drift `leyline-cas-ffi` exists to
    /// prevent). If this fails, DO NOT swap. Uses `leyline-core` as a
    /// dev-dependency only; `cas.rs` itself is untouched here.
    #[test]
    fn golden_vectors_match_leyline_core() {
        use leyline_core::ContentAddressed;

        let unicode = "héllo wörld — 𝔯𝔬𝔰𝔞𝔯𝔶 🌍".as_bytes();
        let all_bytes: Vec<u8> = (0u8..=255).collect();
        let large = vec![0xA5u8; 4096];
        let vectors: &[&[u8]] = &[
            b"",
            b"a",
            b"abc",
            b"rosary",
            b"\x00\x01\x02\x03",
            unicode,
            &all_bytes,
            &large,
        ];

        for v in vectors {
            let rosary_hex = content_hash(v);
            // UFCS + explicit deref: `v` is `&&[u8]`; `*v` is the `&[u8]` the
            // `ContentAddressed for [u8]` impl takes as `&self` (avoids the
            // std::hash::Hash name clash).
            let llo: leyline_core::Hash = ContentAddressed::hash(*v);

            // (1) Byte-for-byte: same 32-byte BLAKE3 digest.
            assert_eq!(
                hex::decode(&rosary_hex).unwrap().as_slice(),
                llo.as_bytes(),
                "digest bytes diverge for input len {}",
                v.len()
            );
            // (2) Wire-format: rosary's lowercase hex == hex of LLO's bytes, so
            //     the swap is string-transparent for every existing caller.
            assert_eq!(
                rosary_hex,
                hex::encode(llo.as_bytes()),
                "hex wire format diverges for input len {}",
                v.len()
            );
        }
    }
}
