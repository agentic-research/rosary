//! Workspace checkpoint and cleanup during pipeline phase transitions.
//!
//! Sentry spans: `reconcile.workspace.checkpoint`, `reconcile.workspace.cleanup`

use super::Reconciler;
use crate::store::WorkRef;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HandoffAttestationMode {
    None,
    Signed,
    UnsignedForensic,
}

fn handoff_attestation_mode(
    attestation: Option<&crate::config::AttestationConfig>,
) -> HandoffAttestationMode {
    match attestation {
        None => HandoffAttestationMode::None,
        Some(config) if config.signing_key_path.is_some() => HandoffAttestationMode::Signed,
        Some(config) if config.emit_unsigned => HandoffAttestationMode::UnsignedForensic,
        Some(_) => HandoffAttestationMode::None,
    }
}

impl Reconciler {
    /// Checkpoint workspace (jj commit + bookmark) without cleanup.
    ///
    /// Used during phase advancement: the workspace stays alive so the
    /// next pipeline agent reuses the same worktree and its changes.
    pub(super) async fn checkpoint_workspace(&mut self, bead_id: &str) -> Option<String> {
        let change_id = if let Some(ws) = self.completed_workspaces.remove(bead_id) {
            let message = format!("[{bead_id}] fix: agent checkpoint");
            let result = ws.checkpoint(&message).await;
            // Put it back — workspace stays for next phase or cleanup
            self.completed_workspaces.insert(bead_id.to_string(), ws);
            match result {
                Ok(Some(id)) => {
                    eprintln!("[checkpoint] {bead_id}: jj change {id}");
                    Some(id)
                }
                Ok(None) => None,
                Err(e) => {
                    eprintln!("[checkpoint] {bead_id}: failed: {e}");
                    None
                }
            }
        } else {
            None
        };

        // Log change_id as event for audit trail
        if let Some(ref cid) = change_id {
            let repo = self
                .trackers
                .get(bead_id)
                .map(|t| t.repo.clone())
                .unwrap_or_default();
            if let Some(client) = self.dolt_client(&repo).await {
                client.log_event(bead_id, "jj_checkpoint", cid).await;
            }
        }

        change_id
    }

    /// Checkpoint workspace then write handoff + manifest, then clean up.
    ///
    /// Used when the pipeline is complete (no next agent) or on deadletter.
    pub(super) async fn checkpoint_and_cleanup(&mut self, bead_id: &str) -> Option<String> {
        let change_id = self.checkpoint_workspace(bead_id).await;

        // Write handoff + manifest to workspace before cleanup
        let repo = self
            .trackers
            .get(bead_id)
            .map(|t| t.repo.clone())
            .unwrap_or_default();
        // Drain pending tool records now so they're consumed on all exit paths below
        // (including the early-return on chain integrity failure).
        let pending_tools = self.pending_tools_used.remove(bead_id).unwrap_or_default();

        if let Some(ws) = self.completed_workspaces.get(bead_id) {
            let work_dir = &ws.work_dir;

            // Build work summary from git
            let work = crate::manifest::Work::from_git(work_dir, change_id.as_deref());

            // Write handoff for the phase that just completed
            let (agent, phase) = self
                .trackers
                .get(bead_id)
                .map(|t| {
                    (
                        t.current_agent
                            .clone()
                            .unwrap_or_else(|| "dev-agent".to_string()),
                        t.phase_index,
                    )
                })
                .unwrap_or_else(|| ("dev-agent".to_string(), 0));
            // For phase > 0, missing previous handoff breaks the chain.
            // Skip handoff write to avoid creating an unlinked attestation.
            let previous = if phase > 0 {
                match crate::handoff::Handoff::read_from(&ws.work_dir, phase - 1) {
                    Ok(h) => Some(h),
                    Err(e) => {
                        eprintln!(
                            "[handoff] {bead_id}: phase {phase} cannot read previous handoff, \
                             skipping handoff write to preserve chain integrity: {e}"
                        );
                        return change_id;
                    }
                }
            } else {
                None
            };
            let mut handoff = crate::handoff::Handoff::new(
                phase,
                &agent,
                None,
                bead_id,
                self.provider.name(),
                &work,
                previous.as_ref(),
            );
            // Look up thread_id from hierarchy if available
            if let Some(ref hierarchy) = self.hierarchy {
                let bead_ref = WorkRef {
                    repo: repo.clone(),
                    scope: String::new(),
                    bead_id: bead_id.to_string(),
                };
                if let Ok(Some(tid)) = hierarchy.find_thread_for_bead(&bead_ref).await {
                    handoff.thread_id = Some(tid);
                }
            }
            if !pending_tools.is_empty() {
                handoff.tools_used = pending_tools;
            }
            // Only emit a DSSE envelope when the handoff itself was successfully
            // written — otherwise the envelope would attest to a non-existent file.
            match handoff.write_to(work_dir) {
                Ok(handoff_path) => {
                    self.write_handoff_envelope(bead_id, &handoff_path, phase, &handoff);
                }
                Err(e) => {
                    eprintln!("[handoff] {bead_id}: failed to write: {e}");
                }
            }

            // Write manifest
            let vcs_kind = match ws.vcs {
                crate::workspace::VcsKind::Jj => "jj",
                crate::workspace::VcsKind::Git => "git",
                crate::workspace::VcsKind::None => "none",
            };
            let mut manifest = crate::manifest::Manifest::at_spawn(
                &format!("d-{bead_id}"),
                bead_id,
                &repo,
                &agent,
                self.provider.name(),
                "task",
                "implement",
                phase,
                &work_dir.display().to_string(),
                &ws.repo_path.display().to_string(),
                vcs_kind,
                None,
            );
            manifest.work = work;
            manifest.complete(true, Some("end_turn"));
            if let Err(e) = manifest.write_to(work_dir) {
                eprintln!("[manifest] {bead_id}: failed to write: {e}");
            }
        }

        // Terminal step: merge or PR based on issue type.
        // Runs outside the workspace borrow scope to allow dolt_client access.
        if let Some(ws) = self.completed_workspaces.get(bead_id) {
            let branch = format!("fix/{bead_id}");
            let ws_repo_path = ws.repo_path.clone();
            let issue_type = if let Some(client) = self.dolt_client(&repo).await {
                client
                    .get_bead(bead_id, &repo)
                    .await
                    .ok()
                    .flatten()
                    .map(|b| b.issue_type)
                    .unwrap_or_else(|| "task".to_string())
            } else {
                "task".to_string()
            };
            // Resolve the PR base: thread feature branch if bead belongs to a thread,
            // otherwise the configured default branch (from `[github] base`).
            // BDR→git: bead PRs into thread branch, standalone beads PR into default.
            let thread_base: Option<String> = if let Some(ref hierarchy) = self.hierarchy {
                let bead_ref = WorkRef {
                    repo: repo.clone(),
                    scope: String::new(),
                    bead_id: bead_id.to_string(),
                };
                if let Ok(Some(thread_id)) = hierarchy.find_thread_for_bead(&bead_ref).await {
                    let decade_id = thread_id.split('/').next().unwrap_or(&thread_id);
                    hierarchy
                        .list_threads(decade_id)
                        .await
                        .ok()
                        .and_then(|threads| {
                            threads
                                .iter()
                                .find(|t| t.id == thread_id)
                                .and_then(|t| t.feature_branch.clone())
                        })
                } else {
                    None
                }
            } else {
                None
            };
            let base = thread_base
                .as_deref()
                .unwrap_or(&self.config.default_branch);

            if let Ok(result) = crate::workspace::merge_or_pr_with_base(
                &ws_repo_path,
                &branch,
                bead_id,
                &issue_type,
                Some(base),
            )
            .await
            {
                // Record PR URL on the bead as comment + event (event used by poll_pr_merges)
                if let Some(ref pr_url) = result.pr_url
                    && let Some(client) = self.dolt_client(&repo).await
                {
                    let _ = client
                        .add_comment(bead_id, &format!("PR: {pr_url}"), "rosary")
                        .await;
                    client.log_event(bead_id, "pr_url", pr_url).await;
                }
            }
        }

        self.cleanup_workspace(bead_id);
        change_id
    }

    /// Clean up the workspace for a completed bead.
    /// Delegates to workspace.rs cleanup functions to avoid duplication.
    /// No-op when no workspace is tracked — avoids touching the real filesystem
    /// for unknown beads (safety: prevents deleting worktrees from other reconcilers).
    pub(super) fn cleanup_workspace(&mut self, bead_id: &str) {
        if let Some(ws) = self.completed_workspaces.remove(bead_id) {
            eprintln!(
                "[cleanup] {bead_id} workspace (vcs={:?}, compute={})",
                ws.vcs,
                self.compute.name()
            );
            match ws.vcs {
                crate::workspace::VcsKind::Jj => {
                    crate::workspace::cleanup_jj_workspace(&ws.repo_path, bead_id);
                }
                crate::workspace::VcsKind::Git => {
                    crate::workspace::cleanup_git_worktree(&ws.repo_path, bead_id);
                }
                crate::workspace::VcsKind::None => {}
            }
        } else {
            // No workspace tracked — skip cleanup. The legacy fallback that
            // cleaned up from "." was unsafe: it could delete worktrees
            // belonging to other reconcilers or from previous runs.
            eprintln!("[cleanup] {bead_id}: no workspace tracked, skipping");
        }
    }

    /// Write signed APAS L2 evidence or explicitly requested unsigned forensic evidence.
    ///
    /// The in-toto subject digest is computed from the **on-disk** handoff bytes
    /// so external observers can verify by hashing the file directly.
    ///
    /// All errors are logged and swallowed — envelope failure must not block
    /// the pipeline since the handoff itself is already on disk.
    pub(super) fn write_handoff_envelope(
        &self,
        bead_id: &str,
        handoff_path: &std::path::Path,
        phase: u32,
        handoff: &crate::handoff::Handoff,
    ) {
        let handoff_predicate = match serde_json::to_value(handoff) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[dsse] {bead_id}: serialize handoff: {e}");
                return;
            }
        };

        let work_dir = handoff_path.parent().unwrap_or(handoff_path);
        match handoff_attestation_mode(self.config.attestation.as_ref()) {
            HandoffAttestationMode::None => {}
            HandoffAttestationMode::Signed => {
                let Some(signer) = self.load_attestation_key(bead_id) else {
                    return;
                };
                let envelope = match crate::handoff_attestation::wrap_handoff_from_file(
                    handoff_path,
                    &handoff_predicate,
                    &signer,
                ) {
                    Ok(env) => env,
                    Err(e) => {
                        eprintln!("[dsse] {bead_id}: wrap handoff: {e}");
                        return;
                    }
                };
                if let Err(e) =
                    crate::handoff_attestation::write_envelope(work_dir, phase, &envelope)
                {
                    eprintln!("[dsse] {bead_id}: write envelope: {e}");
                } else {
                    eprintln!("[dsse] {bead_id}: phase {phase} signed envelope written");
                }
            }
            HandoffAttestationMode::UnsignedForensic => {
                let statement =
                    match crate::handoff_attestation::unsigned_handoff_statement_from_file(
                        handoff_path,
                        &handoff_predicate,
                    ) {
                        Ok(statement) => statement,
                        Err(e) => {
                            eprintln!("[dsse] {bead_id}: build unsigned statement: {e}");
                            return;
                        }
                    };
                if let Err(e) = crate::handoff_attestation::write_unsigned_statement(
                    work_dir, phase, &statement,
                ) {
                    eprintln!("[dsse] {bead_id}: write unsigned statement: {e}");
                } else {
                    eprintln!(
                        "[dsse] {bead_id}: phase {phase} unsigned forensic statement written"
                    );
                }
            }
        }
    }

    /// Resolve and load the attestation signing key, expanding `~` in the path.
    /// Returns None if attestation is not configured or the key cannot be loaded.
    pub(super) fn load_attestation_key(
        &self,
        bead_id: &str,
    ) -> Option<leyline_envelope::Ed25519RootSigner> {
        let key_path = self
            .config
            .attestation
            .as_ref()?
            .signing_key_path
            .as_ref()?;
        let expanded = shellexpand::tilde(&key_path.display().to_string()).into_owned();
        match crate::handoff_attestation::load_signing_key(std::path::Path::new(&expanded)) {
            Ok(k) => Some(k),
            Err(e) => {
                eprintln!("[dsse] {bead_id}: load signing key: {e}");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attestation_mode_requires_explicit_unsigned_opt_in() {
        assert_eq!(handoff_attestation_mode(None), HandoffAttestationMode::None);

        let empty = crate::config::AttestationConfig {
            signing_key_path: None,
            emit_unsigned: false,
        };
        assert_eq!(
            handoff_attestation_mode(Some(&empty)),
            HandoffAttestationMode::None
        );

        let unsigned = crate::config::AttestationConfig {
            signing_key_path: None,
            emit_unsigned: true,
        };
        assert_eq!(
            handoff_attestation_mode(Some(&unsigned)),
            HandoffAttestationMode::UnsignedForensic
        );

        let signed = crate::config::AttestationConfig {
            signing_key_path: Some("missing.key".into()),
            emit_unsigned: true,
        };
        assert_eq!(
            handoff_attestation_mode(Some(&signed)),
            HandoffAttestationMode::Signed
        );
    }

    fn sample_handoff() -> crate::handoff::Handoff {
        crate::handoff::Handoff::new(
            0,
            "dev-agent",
            None,
            "test-bead",
            "claude",
            &crate::manifest::Work::default(),
            None,
        )
    }

    async fn reconciler_with_attestation(
        attestation: Option<crate::config::AttestationConfig>,
    ) -> Reconciler {
        Reconciler::new(super::super::ReconcilerConfig {
            once: true,
            repo: Vec::new(),
            attestation,
            ..Default::default()
        })
        .await
    }

    #[tokio::test]
    async fn write_handoff_envelope_signed_mode_writes_a_verifiable_dsse_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let key_path = tmp.path().join("signing.key");
        std::fs::write(&key_path, [4u8; 32]).unwrap();

        let r = reconciler_with_attestation(Some(crate::config::AttestationConfig {
            signing_key_path: Some(key_path),
            emit_unsigned: false,
        }))
        .await;

        let handoff = sample_handoff();
        let handoff_path = handoff.write_to(tmp.path()).unwrap();
        r.write_handoff_envelope("test-bead", &handoff_path, 0, &handoff);

        let envelope_path = tmp.path().join(".rsry-handoff-0.dsse.json");
        assert!(
            envelope_path.exists(),
            "signed mode must write a .dsse.json envelope"
        );
        let bytes = std::fs::read(&envelope_path).unwrap();
        let envelope = leyline_envelope::Envelope::from_json_slice(&bytes).unwrap();
        let signer = leyline_envelope::Ed25519RootSigner::from_seed(&[4u8; 32]);
        let stmt = envelope.verify(&signer.verifying_key()).unwrap();
        assert_eq!(stmt.predicate()["bead_id"], "test-bead");
        assert!(!tmp.path().join(".rsry-handoff-0.intoto.json").exists());
    }

    #[tokio::test]
    async fn write_handoff_envelope_unsigned_forensic_mode_writes_a_statement_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let r = reconciler_with_attestation(Some(crate::config::AttestationConfig {
            signing_key_path: None,
            emit_unsigned: true,
        }))
        .await;

        let handoff = sample_handoff();
        let handoff_path = handoff.write_to(tmp.path()).unwrap();
        r.write_handoff_envelope("test-bead", &handoff_path, 0, &handoff);

        let statement_path = tmp.path().join(".rsry-handoff-0.intoto.json");
        assert!(
            statement_path.exists(),
            "unsigned-forensic mode must write a .intoto.json statement"
        );
        assert!(!tmp.path().join(".rsry-handoff-0.dsse.json").exists());
    }

    #[tokio::test]
    async fn write_handoff_envelope_none_mode_writes_nothing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let r = reconciler_with_attestation(None).await;

        let handoff = sample_handoff();
        let handoff_path = handoff.write_to(tmp.path()).unwrap();
        r.write_handoff_envelope("test-bead", &handoff_path, 0, &handoff);

        assert!(!tmp.path().join(".rsry-handoff-0.dsse.json").exists());
        assert!(!tmp.path().join(".rsry-handoff-0.intoto.json").exists());
    }

    #[tokio::test]
    async fn load_attestation_key_none_when_attestation_unconfigured() {
        let r = reconciler_with_attestation(None).await;
        assert!(r.load_attestation_key("test-bead").is_none());
    }

    #[tokio::test]
    async fn load_attestation_key_none_when_key_file_missing() {
        let r = reconciler_with_attestation(Some(crate::config::AttestationConfig {
            signing_key_path: Some("/nonexistent/signing.key".into()),
            emit_unsigned: false,
        }))
        .await;
        assert!(r.load_attestation_key("test-bead").is_none());
    }
}
