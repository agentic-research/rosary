//! Bead-level diff between two JSONL snapshots, rendered for human review.
//!
//! **Why this exists (rosary-fa7167 Q1).** The in-tree tracked `beads.jsonl` has
//! exactly one benefit no other surveyed work-tracker offers: a bead change is
//! *reviewable in a PR diff* — "this commit added exactly one bead". That single
//! benefit is what keeps canonical beads in the working tree, and it buys three
//! costs (`docs/prior-art/state-sync-sota.md`): commit-coupling, merge conflicts
//! against code, and a hook that must stage a file. All three were measured
//! biting on 2026-07-27.
//!
//! Q1 asks whether the benefit is *recoverable out of tree*. This module is the
//! experiment: if a readable bead diff can be rendered from a source that is
//! **not** in the working tree (a git rev, or a ref), then reviewability
//! survives a move and the in-tree default loses its sole advantage.
//!
//! Two deliberate design properties:
//!
//! 1. **Source-agnostic.** Snapshots are just JSONL text. Where the text came
//!    from — a file, `git show <rev>:<path>`, `git cat-file` on a ref — is the
//!    caller's problem. That is what makes the out-of-tree question answerable
//!    rather than hypothetical.
//! 2. **Field-generic.** The differ never declares a bead field set; it walks
//!    whatever keys the records carry. So it *cannot* drift from the canonical
//!    field set the way the seven hand-rolled field lists ADR-0021 catalogues
//!    have. Adding a bead field needs no change here.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::text::truncate;

/// Fields whose churn is noise in a review — they change on every write and
/// carry no reviewable intent.
const NOISE_FIELDS: &[&str] = &["updated_at", "comment_count"];

/// One record's worth of change.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldChange {
    pub field: String,
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Changed {
    pub id: String,
    pub title: String,
    pub fields: Vec<FieldChange>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: Option<i64>,
    pub issue_type: String,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct BeadDiff {
    pub added: Vec<Summary>,
    pub removed: Vec<Summary>,
    pub changed: Vec<Changed>,
}

impl BeadDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// Parse a JSONL snapshot into id → record. Blank lines are skipped; a
/// malformed line is an error rather than a silent drop — a differ that
/// quietly ignores records would under-report exactly the change a reviewer
/// needs to see.
pub fn parse_snapshot(jsonl: &str) -> Result<BTreeMap<String, Value>> {
    let mut out = BTreeMap::new();
    for (i, line) in jsonl.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line)
            .with_context(|| format!("parsing bead record on line {}", i + 1))?;
        let id = v
            .get("id")
            .and_then(|v| v.as_str())
            .with_context(|| format!("bead record on line {} has no `id`", i + 1))?
            .to_string();
        out.insert(id, v);
    }
    Ok(out)
}

fn field_str(v: &Value, key: &str) -> String {
    match v.get(key) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

fn summarize(v: &Value) -> Summary {
    Summary {
        id: field_str(v, "id"),
        title: field_str(v, "title"),
        status: field_str(v, "status"),
        priority: v.get("priority").and_then(|p| p.as_i64()),
        issue_type: field_str(v, "issue_type"),
    }
}

/// Diff two snapshots. `before` may legitimately be empty (a first import).
pub fn diff(before: &BTreeMap<String, Value>, after: &BTreeMap<String, Value>) -> BeadDiff {
    let noise: BTreeSet<&str> = NOISE_FIELDS.iter().copied().collect();
    let mut d = BeadDiff::default();

    for (id, rec) in after {
        match before.get(id) {
            None => d.added.push(summarize(rec)),
            Some(old) => {
                // Union of keys, so a field APPEARING or DISAPPEARING is a
                // change — that is precisely the ADR-0021 drift symptom, and a
                // reviewer should see it.
                let keys: BTreeSet<&str> = old
                    .as_object()
                    .into_iter()
                    .flat_map(|m| m.keys().map(|k| k.as_str()))
                    .chain(
                        rec.as_object()
                            .into_iter()
                            .flat_map(|m| m.keys().map(|k| k.as_str())),
                    )
                    .filter(|k| !noise.contains(k))
                    .collect();

                let fields: Vec<FieldChange> = keys
                    .into_iter()
                    .filter_map(|k| {
                        let b = field_str(old, k);
                        let a = field_str(rec, k);
                        (b != a).then(|| FieldChange {
                            field: k.to_string(),
                            before: b,
                            after: a,
                        })
                    })
                    .collect();

                if !fields.is_empty() {
                    d.changed.push(Changed {
                        id: id.clone(),
                        title: field_str(rec, "title"),
                        fields,
                    });
                }
            }
        }
    }

    for (id, rec) in before {
        if !after.contains_key(id) {
            d.removed.push(summarize(rec));
        }
    }

    d
}

/// Escape a value for a markdown table cell.
fn cell(s: &str) -> String {
    let one_line = s.replace('\n', " ").replace('|', "\\|");
    truncate(&one_line, 60)
}

fn pri(p: Option<i64>) -> String {
    p.map(|p| format!("P{p}")).unwrap_or_else(|| "—".into())
}

/// Render as markdown suitable for a PR comment.
///
/// **`removed` is reported loudly.** A bead vanishing from the record is the
/// shape of every data-loss incident in this repo's history (rosary-05fbe0's
/// 1115-record wipe, ley-line-open's 26 stranded beads), and it is the one
/// change a reviewer must never skim past.
pub fn render_markdown(d: &BeadDiff, from: &str, to: &str) -> String {
    let mut out = String::new();
    out.push_str("### Bead changes\n\n");
    out.push_str(&format!("`{}` → `{}`\n\n", cell(from), cell(to)));

    if d.is_empty() {
        out.push_str("_No bead changes._\n");
        return out;
    }

    if !d.removed.is_empty() {
        out.push_str(&format!(
            "> [!WARNING]\n> **{} bead(s) REMOVED from the record.** Every data-loss \
             incident in this repo has this shape — confirm each is intentional.\n\n",
            d.removed.len()
        ));
        out.push_str("| id | pri | status | title |\n|---|---|---|---|\n");
        for b in &d.removed {
            out.push_str(&format!(
                "| `{}` | {} | {} | {} |\n",
                cell(&b.id),
                pri(b.priority),
                cell(&b.status),
                cell(&b.title)
            ));
        }
        out.push('\n');
    }

    if !d.added.is_empty() {
        out.push_str(&format!("**Added ({})**\n\n", d.added.len()));
        out.push_str("| id | pri | type | title |\n|---|---|---|---|\n");
        for b in &d.added {
            out.push_str(&format!(
                "| `{}` | {} | {} | {} |\n",
                cell(&b.id),
                pri(b.priority),
                cell(&b.issue_type),
                cell(&b.title)
            ));
        }
        out.push('\n');
    }

    if !d.changed.is_empty() {
        out.push_str(&format!("**Changed ({})**\n\n", d.changed.len()));
        for c in &d.changed {
            out.push_str(&format!("- `{}` — {}\n", cell(&c.id), cell(&c.title)));
            for f in &c.fields {
                let before = if f.before.is_empty() {
                    "_(empty)_".to_string()
                } else {
                    format!("`{}`", cell(&f.before))
                };
                let after = if f.after.is_empty() {
                    "_(empty)_".to_string()
                } else {
                    format!("`{}`", cell(&f.after))
                };
                out.push_str(&format!("  - **{}**: {} → {}\n", f.field, before, after));
            }
        }
        out.push('\n');
    }

    out.push_str(&format!(
        "_{} added · {} changed · {} removed_\n",
        d.added.len(),
        d.changed.len(),
        d.removed.len()
    ));
    out
}

/// Read a snapshot from a source spec, resolved in this order:
///
///   `-`               stdin
///   `<rev>:<path>`    a git blob — `git show HEAD~1:.beads/beads.jsonl`
///   `<ref>`           a git ref holding JSONL — `refs/beads/main`
///   anything else     a file path
///
/// The git forms are the load-bearing ones: they let bead state be read from
/// somewhere OTHER than the working tree, which is exactly what the
/// in-tree-vs-out-of-tree question needs in order to be answerable rather than
/// hypothetical (rosary-fa7167 Q1).
pub fn read_snapshot(spec: &str, repo_root: &std::path::Path) -> Result<String> {
    use std::io::Read as _;

    if spec == "-" {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        return Ok(buf);
    }

    let run_git = |args: &[&str]| -> Result<Option<String>> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(args)
            .output()?;
        Ok(out
            .status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned()))
    };

    // `<rev>:<path>` — a blob at a revision. A miss falls through to the file
    // path branch, since `:` is legal in a filename.
    if spec.contains(':')
        && let Some(text) = run_git(&["show", spec])?
    {
        return Ok(text);
    }

    // A bare ref whose object IS the JSONL (the out-of-tree shape).
    if spec.starts_with("refs/") {
        if let Some(text) = run_git(&["cat-file", "-p", spec])? {
            return Ok(text);
        }
        anyhow::bail!(
            "git ref `{spec}` is not readable in {}. Note a plain `git clone` \
             fetches only `refs/heads/*`, so custom ref namespaces need an \
             explicit refspec — that cost is rosary-fa7167 Q3.",
            repo_root.display()
        );
    }

    let path = std::path::Path::new(spec);
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo_root.join(path)
    };
    std::fs::read_to_string(&full).with_context(|| format!("reading {}", full.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(records: &[&str]) -> BTreeMap<String, Value> {
        parse_snapshot(&records.join("\n")).unwrap()
    }

    const A_OPEN: &str = r#"{"id":"rosary-aaa111","title":"Fix the thing","status":"open","priority":1,"issue_type":"bug","updated_at":"2026-07-27T10:00:00Z"}"#;
    const A_DONE: &str = r#"{"id":"rosary-aaa111","title":"Fix the thing","status":"done","priority":1,"issue_type":"bug","updated_at":"2026-07-27T18:00:00Z"}"#;
    const B_OPEN: &str = r#"{"id":"rosary-bbb222","title":"Another thing","status":"open","priority":2,"issue_type":"task","updated_at":"2026-07-27T10:00:00Z"}"#;

    #[test]
    fn detects_added_removed_and_changed() {
        let d = diff(&snap(&[A_OPEN]), &snap(&[A_DONE, B_OPEN]));
        assert_eq!(d.added.len(), 1, "B is new");
        assert_eq!(d.added[0].id, "rosary-bbb222");
        assert_eq!(d.changed.len(), 1, "A changed status");
        assert_eq!(d.changed[0].fields.len(), 1, "only status, not updated_at");
        assert_eq!(d.changed[0].fields[0].field, "status");
        assert_eq!(d.changed[0].fields[0].before, "open");
        assert_eq!(d.changed[0].fields[0].after, "done");
        assert!(d.removed.is_empty());
    }

    /// `updated_at` churns on every write; surfacing it would bury real intent.
    #[test]
    fn noise_fields_are_not_reported_as_changes() {
        let a2 = r#"{"id":"rosary-aaa111","title":"Fix the thing","status":"open","priority":1,"issue_type":"bug","updated_at":"2026-07-27T23:59:00Z"}"#;
        let d = diff(&snap(&[A_OPEN]), &snap(&[a2]));
        assert!(d.is_empty(), "timestamp-only change is not a review event");
    }

    #[test]
    fn removal_is_detected_and_warned_loudly() {
        let d = diff(&snap(&[A_OPEN, B_OPEN]), &snap(&[A_OPEN]));
        assert_eq!(d.removed.len(), 1);
        assert_eq!(d.removed[0].id, "rosary-bbb222");
        let md = render_markdown(&d, "before", "after");
        assert!(md.contains("[!WARNING]"), "removal must be loud: {md}");
        assert!(md.contains("REMOVED"));
        assert!(md.contains("rosary-bbb222"));
    }

    /// A field appearing or disappearing is the ADR-0021 drift symptom.
    #[test]
    fn a_field_appearing_or_vanishing_is_a_change() {
        let without = r#"{"id":"x-1","title":"t","status":"open"}"#;
        let with = r#"{"id":"x-1","title":"t","status":"open","acceptance_criteria":"cargo test"}"#;
        let gained = diff(&snap(&[without]), &snap(&[with]));
        assert_eq!(gained.changed.len(), 1);
        assert_eq!(gained.changed[0].fields[0].field, "acceptance_criteria");
        assert_eq!(gained.changed[0].fields[0].before, "");

        let lost = diff(&snap(&[with]), &snap(&[without]));
        assert_eq!(lost.changed.len(), 1, "losing a field must not be silent");
        assert_eq!(lost.changed[0].fields[0].after, "");
    }

    #[test]
    fn empty_before_is_an_all_added_diff_not_an_error() {
        let d = diff(&snap(&[]), &snap(&[A_OPEN, B_OPEN]));
        assert_eq!(d.added.len(), 2);
        assert!(d.removed.is_empty());
    }

    #[test]
    fn identical_snapshots_render_as_no_change() {
        let d = diff(&snap(&[A_OPEN]), &snap(&[A_OPEN]));
        assert!(d.is_empty());
        assert!(render_markdown(&d, "a", "b").contains("_No bead changes._"));
    }

    /// A silently-dropped malformed record would under-report the very change
    /// a reviewer needs. Fail loud instead.
    #[test]
    fn malformed_record_errors_rather_than_being_skipped() {
        assert!(parse_snapshot("{not json}").is_err());
        assert!(
            parse_snapshot(r#"{"title":"no id here"}"#).is_err(),
            "a record without an id must not be silently ignored"
        );
    }

    #[test]
    fn blank_lines_are_tolerated() {
        let s = parse_snapshot(&format!("\n{A_OPEN}\n\n{B_OPEN}\n")).unwrap();
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn markdown_cells_escape_pipes_and_newlines() {
        let weird =
            r#"{"id":"x-1","title":"a | b\nc","status":"open","priority":0,"issue_type":"bug"}"#;
        let d = diff(&snap(&[]), &snap(&[weird]));
        let md = render_markdown(&d, "a", "b");
        assert!(md.contains("a \\| b c"), "pipe/newline not escaped: {md}");
        // The table must stay one row per bead.
        let rows = md.lines().filter(|l| l.starts_with("| `x-1`")).count();
        assert_eq!(rows, 1);
    }
}
