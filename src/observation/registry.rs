//! Field/algebra registry — `FieldName` → `Box<dyn FieldAlgebra>` dispatch.
//!
//! Implementation lands in bead `obs-registry-and-fold` (rosary-980824).
//! Per-process singleton via `OnceLock`, mirroring `src/plugin.rs`.
