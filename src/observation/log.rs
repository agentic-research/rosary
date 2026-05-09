//! In-memory observation log + dedup before fold.
//!
//! Implementation lands in bead `obs-storage-and-quarantine` (rosary-97e386).
//! ADR-0010 invariant 8. Dedup key is `(Source, source_event_id, payload_hash)`.
