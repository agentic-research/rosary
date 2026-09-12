//! The post-merge template (trunk refresh lands here: rosary-e5c0a0).

use super::*;

fn post_merge_template() -> &'static str {
    HOOKS
        .iter()
        .find(|(n, _)| *n == "post-merge")
        .map(|(_, b)| *b)
        .expect("post-merge template")
}

#[test]
fn post_merge_block_is_portable_and_discloses_installer_version() {
    // The installing binary may live in an ephemeral cargo target or
    // worktree. Generated hooks must not pin that path: they resolve a
    // durable install at runtime and carry the installer version so a
    // mismatch can be diagnosed.
    let ephemeral = "/private/tmp/rosary-build/target/debug/rsry";
    let rendered = render_block(post_merge_template());
    assert!(
        !rendered.contains(ephemeral),
        "ephemeral installer path must not be baked into the hook"
    );
    assert!(
        !rendered.contains("__RSRY_VERSION__"),
        "the install-time version placeholder must be fully substituted"
    );
    assert!(
        rendered.contains("command -v rsry"),
        "runtime PATH lookup must remain"
    );
    assert!(
        rendered.contains("$HOME/.local/bin/rsry"),
        "PATH-restricted hooks need a stable per-user fallback"
    );
    assert!(
        rendered.contains(&format!(
            "RSRY_HOOK_VERSION=\"{}\"",
            env!("CARGO_PKG_VERSION")
        )),
        "hook must disclose the installer version to its runtime"
    );
}
