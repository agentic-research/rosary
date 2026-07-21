//! `bead-genesis/v1` — the immutable creation blob whose content-address IS the
//! bead's identity (ADR-0020 Layer 1; docs/design/findability-by-identity.md §2).
//!
//! A bead today is addressed by a chain of mutable location facts (path →
//! `.beads/` → backend → tenant), which is why `git reset` could revert live
//! state (rosary-05fbe0), a symlink alias missed the repo pool (rosary-617010),
//! and 18 beads stranded in a phantom store (rosary-560953). The fix is to give
//! a bead a first-class identity that no storage move can change:
//!
//!   `BeadId = blake3(canonical genesis blob)`
//!
//! The genesis blob fixes *creation* facts once, forever — role, `home` claim,
//! creator identity, and a nonce so two beads with identical facts at the same
//! instant are still distinct. Because the serialization is deterministic and
//! the digest is the substrate-locked BLAKE3 (`cas::content_hash` →
//! `leyline-core::ContentAddressed::hash`), rosary and cloister compute the
//! *same* `BeadId` for the same blob (cloister ADR-0003 Layer 1 / ADR-0052).
//!
//! This module is the pure identity primitive: type + canonical form + digest.
//! Wiring it into bead creation, the `resolve(address, context)` seam, and the
//! backfill of existing beads are the follow-on steps of ADR-0020 P1.

use std::collections::BTreeMap;

/// A bead's role, fixed at genesis (ADR-0020 §3). Sharing, git-visibility, and
/// coordination namespace all *derive* from this — not from whichever store the
/// bead happens to sit in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Canonical work-record — the shared, authoritative bead.
    Canonical,
    /// Local coordination record (e.g. a feature-branch / agent-dispatch note).
    Coordination,
    /// Personal record — never leaves its owner's substrate.
    Personal,
}

impl Role {
    /// Canonical lowercase token used in the genesis serialization.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Canonical => "canonical",
            Role::Coordination => "coordination",
            Role::Personal => "personal",
        }
    }
}

/// The immutable creation blob. Its canonical serialization's content-address is
/// the `BeadId`; every field here is fixed at creation and never changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeadGenesis {
    /// Role at genesis (ADR-0020 §3).
    pub role: Role,
    /// `home` claim in `ScopeId` canonical form (e.g. `repo:rosary`). A *claim*,
    /// not a storage address — the whole point is that identity ≠ location.
    pub home: String,
    /// Creator's signet cert fingerprint (attribution; ADR-0010 "attribution =
    /// signet cert"). Empty string for a legacy/backfilled bead with no cert.
    pub creator: String,
    /// 128-bit nonce so identical creation facts still yield distinct beads.
    pub entropy: [u8; 16],
}

impl BeadGenesis {
    /// Schema tag carried in the blob so the format can evolve without silent
    /// digest drift.
    pub const SCHEMA: &'static str = "bead-genesis/v1";

    /// Construct a genesis blob with a fresh random 128-bit nonce (a v4 UUID's
    /// 16 random bytes — `uuid` is already a runtime dep; `rand` is dev-only).
    pub fn new(role: Role, home: impl Into<String>, creator: impl Into<String>) -> Self {
        Self {
            role,
            home: home.into(),
            creator: creator.into(),
            entropy: *uuid::Uuid::new_v4().as_bytes(),
        }
    }

    /// Construct with an explicit nonce — for backfill (deterministic entropy
    /// derived from a bead's recorded facts) and for reproducible tests.
    pub fn with_entropy(
        role: Role,
        home: impl Into<String>,
        creator: impl Into<String>,
        entropy: [u8; 16],
    ) -> Self {
        Self {
            role,
            home: home.into(),
            creator: creator.into(),
            entropy,
        }
    }

    /// Canonical bytes: sorted-key JSON, UTF-8, no trailing newline (cloister
    /// ADR-0003 "Bead canonical form"). Deterministic — the only way two
    /// substrates produce identical digests for the same logical bead.
    ///
    /// `BTreeMap` guarantees keys are emitted in sorted order regardless of
    /// serde_json's `preserve_order` feature, so the digest can never drift on
    /// a downstream feature-flag change.
    pub fn to_canonical_bytes(&self) -> Vec<u8> {
        let mut m: BTreeMap<&'static str, String> = BTreeMap::new();
        m.insert("schema", Self::SCHEMA.to_string());
        m.insert("role", self.role.as_str().to_string());
        m.insert("home", self.home.clone());
        m.insert("creator", self.creator.clone());
        m.insert("entropy", hex::encode(self.entropy));
        serde_json::to_vec(&m).expect("BTreeMap<&str, String> always serializes")
    }

    /// `BeadId = blake3 content-address of the genesis blob`, via the
    /// substrate-locked `cas::content_hash`. Identical for identical genesis on
    /// any substrate (rosary, cloister) — the ADR-0052 convergence invariant.
    pub fn bead_id(&self) -> String {
        crate::cas::content_hash(&self.to_canonical_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(entropy: [u8; 16]) -> BeadGenesis {
        BeadGenesis::with_entropy(Role::Canonical, "repo:rosary", "signet:abcd1234", entropy)
    }

    #[test]
    fn role_tokens_are_canonical_lowercase() {
        assert_eq!(Role::Canonical.as_str(), "canonical");
        assert_eq!(Role::Coordination.as_str(), "coordination");
        assert_eq!(Role::Personal.as_str(), "personal");
    }

    #[test]
    fn canonical_bytes_have_sorted_keys_and_no_trailing_newline() {
        let bytes = sample([0u8; 16]).to_canonical_bytes();
        let s = std::str::from_utf8(&bytes).unwrap();
        // Keys must appear sorted: creator < entropy < home < role < schema.
        let expected = concat!(
            "{",
            "\"creator\":\"signet:abcd1234\",",
            "\"entropy\":\"00000000000000000000000000000000\",",
            "\"home\":\"repo:rosary\",",
            "\"role\":\"canonical\",",
            "\"schema\":\"bead-genesis/v1\"",
            "}"
        );
        assert_eq!(
            s, expected,
            "canonical form must be sorted-key JSON, single line"
        );
    }

    #[test]
    fn bead_id_is_deterministic_64_hex() {
        let a = sample([7u8; 16]);
        let b = sample([7u8; 16]);
        let id = a.bead_id();
        assert_eq!(id, b.bead_id(), "identical genesis → identical BeadId");
        assert_eq!(id.len(), 64, "blake3-256 hex = 64 chars");
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn nonce_distinguishes_otherwise_identical_beads() {
        // Same title/role/home/creator + same instant must still be distinct
        // work items — the entropy nonce is what guarantees it.
        let a = sample([1u8; 16]);
        let b = sample([2u8; 16]);
        assert_ne!(
            a.bead_id(),
            b.bead_id(),
            "different nonce → different BeadId"
        );
    }

    #[test]
    fn bead_id_matches_cas_of_canonical_bytes() {
        // The BeadId is exactly the substrate digest of the canonical blob — no
        // extra framing — so cloister can reproduce it byte-for-byte.
        let g = sample([9u8; 16]);
        assert_eq!(
            g.bead_id(),
            crate::cas::content_hash(&g.to_canonical_bytes())
        );
    }

    #[test]
    fn new_generates_distinct_nonces() {
        let a = BeadGenesis::new(Role::Personal, "repo:rosary", "");
        let b = BeadGenesis::new(Role::Personal, "repo:rosary", "");
        assert_ne!(
            a.entropy, b.entropy,
            "fresh genesis must draw a fresh nonce"
        );
    }
}
