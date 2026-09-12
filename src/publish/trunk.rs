//! Trunk refresh: on the trunk, after a merge, re-render every published bead
//! from the store and commit the projection as rosary (P4).
//!
//! Scaffold stub (rosary-e5befe): the surface exists so the wave can be
//! dispatched in parallel; rosary-e5c0a0 fills it in.

use anyhow::{Result, bail};

pub fn refresh_trunk(_repo_root: &std::path::Path, _push: bool) -> Result<()> {
    bail!(
        "rsry bead trunk-refresh: not implemented yet — rosary-e5c0a0 (thread trusted-kernel/projection-timing)"
    )
}
