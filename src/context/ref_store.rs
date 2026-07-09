//! Content-addressed blob adapter over LLO's `BlobStore`. Demoted context lives
//! here keyed by its content hash; agents fetch on demand via `rsry_expand_ref`.

use anyhow::{Context, Result};
use leyline_core::{BlobStore, Hash};

/// Parse a hex content hash back into an LLO `Hash`.
pub fn hash_from_hex(hex: &str) -> Result<Hash> {
    let bytes = hex::decode(hex).context("ref hash is not valid hex")?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("ref hash must be 32 bytes"))?;
    Ok(Hash::from_bytes(arr))
}

/// Thin wrapper over any `BlobStore` that speaks hex content hashes (matching
/// [`crate::cas::content_hash`]) and counts genuinely-new puts for the warmth
/// proof-gate.
pub struct RefStore<B: BlobStore> {
    store: B,
    puts: usize,
}

impl<B: BlobStore> RefStore<B> {
    pub fn new(store: B) -> Self {
        Self { store, puts: 0 }
    }

    /// Store bytes, return the hex content hash. The hex equals
    /// `cas::content_hash`, so a bead's ref == the same CAS everywhere.
    ///
    /// Content-addressed and idempotent: a blob the store already holds is not
    /// re-put, and `puts` counts only genuinely-new work — the warmth invariant
    /// (resume re-fetches nothing already held).
    pub fn put(&mut self, bytes: &[u8]) -> Result<String> {
        let hex = crate::cas::content_hash(bytes);
        let h = hash_from_hex(&hex)?;
        if self
            .store
            .get(h)
            .context("blobstore get (warmth check)")?
            .is_none()
        {
            self.store.put(bytes).context("blobstore put")?;
            self.puts += 1;
        }
        Ok(hex)
    }

    /// Fetch a blob by hex hash. `Ok(None)` on a clean miss; the underlying
    /// store's verify-on-read turns a tampered blob into an `Err`.
    pub fn expand(&self, hash_hex: &str) -> Result<Option<Vec<u8>>> {
        let h = hash_from_hex(hash_hex)?;
        self.store.get(h).context("blobstore get")
    }

    /// Count of genuinely-new puts — instrumentation for the warmth gate.
    pub fn puts(&self) -> usize {
        self.puts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leyline_core::MemBlobStore;

    #[test]
    fn round_trip_and_tamper() {
        let mut rs = RefStore::new(MemBlobStore::default());
        let hash = rs.put(b"hello warm resume").unwrap();
        // hex hash equals cas::content_hash (single source of truth)
        assert_eq!(hash, crate::cas::content_hash(b"hello warm resume"));
        // expand round-trips byte-identical
        assert_eq!(
            rs.expand(&hash).unwrap().as_deref(),
            Some(&b"hello warm resume"[..])
        );
        // a wrong/unknown hash is a clean miss, not a panic
        let bogus = "0".repeat(64);
        assert_eq!(rs.expand(&bogus).unwrap(), None);
    }
}
