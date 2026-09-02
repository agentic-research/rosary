//! Shared helpers for the `rsry` CLI integration-test binaries.
//!
//! One definition per helper name — the mache `duplicate_definitions` smell
//! gate keys on token names globally, so per-binary copies of these helpers
//! register as structural duplicates (rosary-451a9a). Names here must be
//! globally unique across the crate (`run`/`git` collide with src free
//! functions), and each binary includes this file under a binary-unique
//! module alias (`#[path = "common/mod.rs"] mod <binary>_common;`) because
//! a shared `mod common;` token would itself register as a duplicate.

#![allow(dead_code)] // not every test binary uses every helper

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Path to the compiled `rsry` binary under test.
pub fn rsry() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rsry"))
}

/// Run `rsry` with an isolated HOME (so global config/registry are sandboxed).
pub fn rsry_run(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(rsry())
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .output()
        .unwrap()
}

/// Run a git command, asserting success.
pub fn git_ok(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Extract the created bead id from `rsry bead create` stdout (strips ANSI).
pub fn created_id(output: &Output) -> String {
    let mut clean = String::from_utf8_lossy(&output.stdout).into_owned();
    while let Some(start) = clean.find('\x1b') {
        let end = clean[start..]
            .find('m')
            .map_or(start + 1, |i| start + i + 1);
        clean.replace_range(start..end, "");
    }
    clean
        .split_whitespace()
        .find(|word| {
            word.rsplit_once('-').is_some_and(|(_, suffix)| {
                suffix.len() == 6 && suffix.chars().all(|c| c.is_ascii_hexdigit())
            })
        })
        .unwrap_or_else(|| panic!("no bead id in output: {clean}"))
        .to_string()
}
