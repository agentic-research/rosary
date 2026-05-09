//! Cert-validity filter + queryable quarantine surface.
//!
//! Implementation lands in bead `obs-storage-and-quarantine` (rosary-97e386).
//! ADR-0010 invariants 11-12. Phase 1 cert validation is a stub that always
//! returns `Ok` for `cert: None`; signet integration is Phase 2.
