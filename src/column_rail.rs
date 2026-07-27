//! A mechanical rail: every column the row-mapper READS must be SELECTed.
//!
//! ## Why this exists
//!
//! `acceptance_criteria` was written correctly to Dolt and read back as `""`
//! for months, because `search_beads`'s hand-rolled `SELECT` omitted the column
//! while the row-mapper read it with `.try_get(..).unwrap_or_default()`. The
//! missing column produced an empty string instead of an error.
//!
//! That was not merely a missing field. `has_close_condition()` takes
//! `acceptance_criteria` from whatever the read returned, so on that path the
//! close gate saw `""` and **silently fell through to a different rubric**
//! (`test_files`, or a runnable command in the description). Closes succeeded
//! and looked correct, by a route nobody chose. Nothing in the output
//! distinguished "checked your criteria" from "checked something else".
//!
//! Reported by a cloister session (2026-07-27) and filed as `rosary-a03a0c`.
//! **The rail below is that session's suggestion**, and it is better than
//! fixing the one column: it compares the two lists that drifted, so it catches
//! every future occurrence rather than this instance.
//!
//! ## What it checks
//!
//! Source-level, at test time: for each `SELECT ... FROM issues` that feeds a
//! `Bead`, every field the mapper reads via `try_get("x")` must appear in the
//! column list. This is the enforceable form of ADR-0021 slice 1 ("one column
//! list, one row mapper") for the Dolt backend — it does not unify the queries,
//! but it makes their divergence a build failure.
//!
//! It is deliberately a SOURCE scan, not a runtime check: it needs no Dolt
//! server, so it runs in CI where `dolt` is absent.

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    /// Every source that hand-rolls a bead `SELECT`. Adding a backend means
    /// adding it here — the rail is only as wide as this list, and a backend
    /// absent from it is unguarded.
    const SOURCES: &[(&str, &str)] = &[
        ("dolt/query.rs", include_str!("dolt/query.rs")),
        ("dolt/bead_crud.rs", include_str!("dolt/bead_crud.rs")),
        ("bead_sqlite/mod.rs", include_str!("bead_sqlite/mod.rs")),
    ];

    /// Every field name a row-mapper reads — what it EXPECTS the query to
    /// have projected. Dolt uses `try_get("x")`; SQLite uses `row.get("x")`.
    fn mapper_fields(src: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for pat in ["try_get(\"", "row.get(\"", "get::<_, Option<String>>(\""] {
            let mut rest = src;
            while let Some(i) = rest.find(pat) {
                rest = &rest[i + pat.len()..];
                if let Some(end) = rest.find('"') {
                    out.insert(rest[..end].to_string());
                }
            }
        }
        // Count columns come from COALESCE/subquery aliases, not `issues`.
        for derived in ["dep_count", "dependency_count", "comment_count"] {
            out.remove(derived);
        }
        out
    }

    /// Column tokens mentioned in a `SELECT … FROM issues` block.
    fn selected_columns(select_block: &str) -> BTreeSet<String> {
        select_block
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// Extract each `SELECT …` block that reads from `issues`.
    fn bead_selects(src: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = src;
        while let Some(i) = rest.find("SELECT") {
            rest = &rest[i..];
            let end = rest
                .find("FROM issues")
                .or_else(|| rest.find("FROM issues i"));
            match end {
                Some(e) if e < 1200 => out.push(rest[..e].to_string()),
                _ => {}
            }
            rest = &rest["SELECT".len()..];
        }
        out
    }

    /// The rail. A column the mapper reads but a query never selects yields
    /// `""` silently — the `rosary-a03a0c` defect.
    #[test]
    fn every_mapper_field_is_selected_by_every_bead_query() {
        let mut expected: BTreeSet<String> = BTreeSet::new();
        for (_, src) in SOURCES {
            expected.extend(mapper_fields(src));
        }
        // Only real `issues` columns — mappers also read join aliases and
        // fields synthesised elsewhere.
        let issues_columns: BTreeSet<&str> = [
            "id",
            "title",
            "description",
            "design",
            "acceptance_criteria",
            "notes",
            "status",
            "priority",
            "issue_type",
            "assignee",
            "external_ref",
            "user_id",
            "created_by",
            "scope",
            "created_at",
            "updated_at",
        ]
        .into_iter()
        .collect();
        expected.retain(|f| issues_columns.contains(f.as_str()));
        assert!(
            expected.contains("acceptance_criteria"),
            "sanity: the mapper should read acceptance_criteria"
        );

        let mut failures = Vec::new();
        for (file, src) in SOURCES {
            for block in bead_selects(src) {
                // Only FULL-BEAD reads — those that feed a row-mapper. A
                // narrow lookup (`SELECT id, title, description WHERE id = ?`)
                // never constructs a Bead, so holding it to the mapper's field
                // set would make the rail noisy, and a noisy rail is the
                // 0.9%-precision failure this repo is already paying for.
                // `priority` + `issue_type` together mark a full projection:
                // no lookup needs both.
                if !(block.contains("priority") && block.contains("issue_type")) {
                    continue;
                }
                let selected = selected_columns(&block);
                for field in &expected {
                    if !selected.contains(field) {
                        let head: String = block.chars().take(70).collect();
                        failures.push(format!(
                            "{file}: a SELECT projecting a bead omits `{field}` \
                             (mapper reads it, so it will silently be \"\") — {head}…"
                        ));
                    }
                }
            }
        }

        assert!(
            failures.is_empty(),
            "SELECT/row-mapper drift — this is rosary-a03a0c's shape:\n  {}",
            failures.join("\n  ")
        );
    }

    /// The rail must be able to FAIL — a rail that cannot fail is the 0.9%-
    /// precision problem one level down.
    #[test]
    fn the_rail_detects_a_missing_column() {
        let selected = selected_columns("SELECT id, title, status FROM issues");
        assert!(selected.contains("title"));
        assert!(
            !selected.contains("acceptance_criteria"),
            "a SELECT without the column must not appear to contain it"
        );
    }
}
