//! Deterministic skill discovery for dispatched agents (rosary-cf52cf).
//!
//! Friction #1 from the determinism-friction log: `/pr-review-kit` existed at
//! `{agents_dir}/skills/pr-review-kit/SKILL.md`, but the agent didn't advertise
//! it — the orchestrator had to find and pass the file by hand. Correctness
//! depended on the model *remembering* a skill was available.
//!
//! This resolves a skill by NAME to its `SKILL.md` path + an immutable content
//! digest, so a dispatch can reference a skill by name and fail loudly *before*
//! spawning if it can't resolve — the #248 conversion rule ("named review
//! harness + immutable skill digest", not prompt assembly).

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// A resolved skill: its `SKILL.md` and a content digest (blake3 hex). The
/// digest pins the exact skill content a dispatch was launched against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRef {
    pub name: String,
    pub path: PathBuf,
    pub digest: String,
}

/// The conventional location of a skill under an agents directory:
/// `{agents_dir}/skills/{name}/SKILL.md`.
pub fn skill_path(agents_dir: &Path, name: &str) -> PathBuf {
    agents_dir.join("skills").join(name).join("SKILL.md")
}

/// Resolve a single skill by name, or fail with a deterministic, actionable
/// error naming the skill and where it was looked for.
pub fn resolve_skill(agents_dir: &Path, name: &str) -> Result<SkillRef> {
    let path = skill_path(agents_dir, name);
    let content = std::fs::read(&path).with_context(|| {
        format!(
            "skill `{name}` did not resolve: no SKILL.md at {} (skill root: {}/skills). \
             Register the skill (or a symlink to it) there, or fix the skill name, before \
             dispatch.",
            path.display(),
            agents_dir.display()
        )
    })?;
    let digest = crate::cas::content_hash(&content);
    Ok(SkillRef {
        name: name.to_string(),
        path,
        digest,
    })
}

/// Resolve every named skill. Fails on the FIRST that can't resolve, so a
/// missing skill is a deterministic pre-dispatch error rather than a mid-run
/// surprise. Returns the resolved refs (name + path + digest) in order.
pub fn resolve_required_skills(agents_dir: &Path, names: &[String]) -> Result<Vec<SkillRef>> {
    names.iter().map(|n| resolve_skill(agents_dir, n)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(root: &Path, name: &str, body: &str) {
        let dir = root.join("skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), body).unwrap();
    }

    #[test]
    fn resolves_existing_skill_with_content_digest() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(tmp.path(), "pr-review-kit", "# PR Review Kit\nreview steps");
        let sk = resolve_skill(tmp.path(), "pr-review-kit").unwrap();
        assert_eq!(sk.name, "pr-review-kit");
        assert!(sk.path.ends_with("skills/pr-review-kit/SKILL.md"));
        // digest is the LLO content-address of the file bytes (stable, shared).
        assert_eq!(
            sk.digest,
            crate::cas::content_hash(b"# PR Review Kit\nreview steps")
        );
    }

    #[test]
    fn missing_skill_fails_deterministically_with_name() {
        let tmp = tempfile::tempdir().unwrap();
        let err = resolve_skill(tmp.path(), "ghost-kit").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("ghost-kit") && msg.contains("did not resolve"),
            "error must name the unresolved skill; got: {msg}"
        );
    }

    #[test]
    fn digest_changes_with_content() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(tmp.path(), "a", "one");
        let d1 = resolve_skill(tmp.path(), "a").unwrap().digest;
        write_skill(tmp.path(), "a", "two");
        let d2 = resolve_skill(tmp.path(), "a").unwrap().digest;
        assert_ne!(d1, d2, "digest must track skill content");
    }

    #[test]
    fn resolve_required_fails_on_first_missing() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(tmp.path(), "present", "ok");
        let err =
            resolve_required_skills(tmp.path(), &["present".to_string(), "absent".to_string()])
                .unwrap_err();
        assert!(err.to_string().contains("absent"), "got: {err}");

        let ok = resolve_required_skills(tmp.path(), &["present".to_string()]).unwrap();
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].name, "present");
    }
}
