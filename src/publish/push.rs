//! pre-push gate over the PUSHED ref's blob: for the beads the pushed commits name,
//! `<local sha>:.beads/beads.jsonl` must agree with the store (P3).
//!
//! Scaffold stub (rosary-e5befe): the surface exists so the wave can be
//! dispatched in parallel; rosary-e5c037 fills it in.

use anyhow::{Result, bail};

pub fn verify_pushed(
    _repo_root: &std::path::Path,
    _prepush_stdin: impl std::io::Read,
) -> Result<()> {
    bail!(
        "rsry bead verify-pushed: not implemented yet — rosary-e5c037 (thread trusted-kernel/projection-timing)"
    )
}
