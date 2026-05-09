//! Flat-lattice algebra with explicit `⊤ = Conflict { values, sources }`,
//! used for cross-source [`crate::observation::FieldName::Status`] derivation.
//!
//! Implementation lands in bead `obs-registry-and-fold` (rosary-980824).
//! ADR-0010 invariant 10. NOT a primitive field algebra — only used to detect
//! cross-source disagreement at fold time.
