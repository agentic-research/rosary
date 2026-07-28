//! Correcting a wrongly-recorded bead status (rosary-e0e19f).
//!
//! Its own module because a correction is not an ordinary bead operation and
//! should not read like one. It is also what kept `bead_ops.rs` inside Golden
//! Rule 2 — a new file rather than a split, since renaming a file invalidates
//! every baselined finding keyed to its old path (learned the hard way while the
//! smell baseline is frozen, rosary-99cfaa).

use crate::store::BeadStore;

/// Correct a bead's recorded status. **Not a transition** (rosary-e0e19f).
///
/// `BeadState::Done` has no valid transitions, which conflates two different
/// claims: *the workflow has no next step*, and *the record cannot be wrong*.
/// The second is false, and it cost real work — the auto-close sweep set beads
/// to `done` with acceptance criteria unmet, including its own bug report, and
/// recovery required a raw `UPDATE` on `beads.db` because `bead reopen` refuses
/// `done`, the CLI had no `bead update`, and `rsry_bead_update` carries no
/// status field (a gap `field_drift` records).
///
/// So this deliberately does NOT consult `can_transition_to`. A correction
/// asserts the previously recorded state was never true; it is not a claim that
/// the machine may move that way. Terminality is preserved for the workflow and
/// the audit trail absorbs the amendment.
///
/// A `reason` is REQUIRED and recorded as a comment carrying old and new values.
/// An untraceable status rewrite would be strictly worse than the raw SQL it
/// replaces: at least the SQL left no impression of having been sanctioned.
pub async fn correct_status<S: BeadStore + ?Sized>(
    store: &S,
    id: &str,
    to: &str,
    reason: &str,
) -> anyhow::Result<()> {
    let reason = reason.trim();
    anyhow::ensure!(
        reason.len() >= 10,
        "a status correction needs a real reason (at least 10 characters) — it \
         overrides the state machine, so the audit trail is the only thing left \
         explaining why"
    );
    let target = crate::bead::BeadState::from(to).to_string();
    let current = store
        .get_status(id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no such bead: {id}"))?;
    if current == target {
        return Ok(());
    }
    // Status FIRST, then the audit comment. The other order writes a comment
    // claiming a correction the write then rejects — which is exactly what the
    // first version of this did, leaving a bead `done` with a comment asserting
    // it had been reopened. A missing comment is a gap; a comment that lies is
    // worse than the raw SQL this replaces.
    store.set_status_verbatim(id, &target).await?;
    store
        .add_comment(
            id,
            &format!("Status corrected {current} → {target} (not a transition). Reason: {reason}"),
            "rsry-correct",
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rosary-e0e19f case end to end: a bead wrongly recorded `done` — the
    /// state `reopen` refuses, and one `BeadState::Done`'s empty transition list
    /// makes unreachable — is corrected, with the reason on the audit trail.
    #[tokio::test]
    async fn correct_status_recovers_a_wrongly_done_bead() {
        use crate::store::BeadStore;
        let store =
            crate::bead_sqlite::SqliteBeadStore::connect(std::path::Path::new(":memory:")).unwrap();
        store
            .create_bead("r-1", "wrongly closed", "", 1, "bug")
            .await
            .unwrap();
        store.set_status_verbatim("r-1", "done").await.unwrap();
        assert!(
            !crate::bead::BeadState::Done.can_transition_to(crate::bead::BeadState::Open),
            "precondition: Done stays terminal for the state machine"
        );

        correct_status(
            &store,
            "r-1",
            "open",
            "auto-closed with acceptance criteria unmet",
        )
        .await
        .unwrap();

        assert_eq!(
            store.get_status("r-1").await.unwrap().as_deref(),
            Some("open")
        );
        let comments = store.list_comments("r-1", false).await.unwrap();
        let last = comments.last().expect("audit comment recorded");
        assert!(
            last.text.contains("done") && last.text.contains("open"),
            "{}",
            last.text
        );
        assert!(
            last.text.contains("acceptance criteria unmet"),
            "{}",
            last.text
        );
    }

    /// A correction overrides the state machine, so the audit trail is all that
    /// explains it. A throwaway reason is refused, and nothing changes.
    #[tokio::test]
    async fn correct_status_demands_a_real_reason() {
        use crate::store::BeadStore;
        let store =
            crate::bead_sqlite::SqliteBeadStore::connect(std::path::Path::new(":memory:")).unwrap();
        store.create_bead("r-2", "t", "", 1, "bug").await.unwrap();
        store.set_status_verbatim("r-2", "done").await.unwrap();
        let err = correct_status(&store, "r-2", "open", "oops")
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("real reason"), "{err}");
        assert_eq!(
            store.get_status("r-2").await.unwrap().as_deref(),
            Some("done"),
            "a refused correction must not change the status"
        );
    }

    /// A no-op correction must NOT leave a comment asserting a change happened.
    #[tokio::test]
    async fn correct_status_is_a_noop_when_already_correct() {
        use crate::store::BeadStore;
        let store =
            crate::bead_sqlite::SqliteBeadStore::connect(std::path::Path::new(":memory:")).unwrap();
        store.create_bead("r-3", "t", "", 1, "bug").await.unwrap();
        correct_status(&store, "r-3", "open", "already open, nothing to fix here")
            .await
            .unwrap();
        assert!(
            store.list_comments("r-3", false).await.unwrap().is_empty(),
            "a no-op must not claim a change on the audit trail"
        );
    }

    #[tokio::test]
    async fn correct_status_rejects_an_unknown_bead() {
        let store =
            crate::bead_sqlite::SqliteBeadStore::connect(std::path::Path::new(":memory:")).unwrap();
        let err = correct_status(&store, "nope", "open", "this bead does not exist at all")
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("no such bead"), "{err}");
    }
}
