//! Bounded, content-addressed pipeline context — warm-resume (rosary-dd5828).
//!
//! "No context bloat" and "warm resume" are one mechanism: the pipeline carries
//! a bounded working set plus content-addressed *references* to demoted material
//! instead of inlining the whole handoff chain into every phase's prompt.

pub mod envelope;
pub mod policy;
pub mod ref_store;
