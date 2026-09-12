//! Commit-scoped publication: the commit-msg hook publishes exactly the beads the
//! commit subject names (thread trusted-kernel/projection-timing, P2).
//!
//! Scaffold stub (rosary-e5befe): the surface exists so the wave can be
//! dispatched in parallel; rosary-e5bfd3 fills it in.

use anyhow::{Result, bail};

pub fn publish_from_commit(
    _repo_root: &std::path::Path,
    _commit_msg: Option<&std::path::Path>,
    _ids: &[String],
) -> Result<()> {
    bail!(
        "rsry bead publish: not implemented yet — rosary-e5bfd3 (thread trusted-kernel/projection-timing)"
    )
}
