//! Deterministic agent harness for dispatch and pipeline tests.
//!
//! The fake provider models a native provider: it consumes `AgentRunSpec`,
//! exposes an optional provider-native `AgentSessionRef`, and never needs a
//! real AI process. Scripted file/commit actions are intentionally explicit so
//! tests can pin pipeline behavior without relying on prompt-following.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::PermissionProfile;
use super::providers::{AgentProvider, AgentRunSpec};
use super::session::{AgentSession, AgentSessionRef};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeterministicAgentAction {
    WriteFile {
        file: PathBuf,
        contents: String,
    },
    CommitWithBeadRef {
        bead_id: String,
        file: PathBuf,
        contents: String,
    },
    CommitPlain {
        message: String,
        file: PathBuf,
        contents: String,
    },
}

#[derive(Debug, Clone)]
pub struct DeterministicAgentProvider {
    name: String,
    exit_success: bool,
    session_ref: Option<AgentSessionRef>,
    actions: Vec<DeterministicAgentAction>,
    captured_specs: Arc<Mutex<Vec<AgentRunSpec>>>,
}

impl DeterministicAgentProvider {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            exit_success: true,
            session_ref: None,
            actions: Vec::new(),
            captured_specs: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn failing(mut self) -> Self {
        self.exit_success = false;
        self
    }

    pub fn with_session_ref(mut self, provider: impl Into<String>, id: impl Into<String>) -> Self {
        self.session_ref = Some(AgentSessionRef::new(provider, id));
        self
    }

    pub fn with_action(mut self, action: DeterministicAgentAction) -> Self {
        self.actions.push(action);
        self
    }

    pub fn captured_specs(&self) -> Arc<Mutex<Vec<AgentRunSpec>>> {
        self.captured_specs.clone()
    }
}

impl AgentProvider for DeterministicAgentProvider {
    fn spawn_run(&self, spec: &AgentRunSpec) -> Result<Box<dyn AgentSession>> {
        self.captured_specs.lock().unwrap().push(spec.clone());
        for action in &self.actions {
            apply_action(&spec.work_dir, action)?;
        }
        Ok(Box::new(DeterministicAgentSession {
            exit_success: self.exit_success,
            session_ref: self.session_ref.clone(),
        }))
    }

    fn spawn_agent(
        &self,
        _prompt: &str,
        _work_dir: &Path,
        _permissions: &PermissionProfile,
        _system_prompt: &str,
    ) -> Result<Box<dyn AgentSession>> {
        anyhow::bail!("deterministic agent harness requires spawn_run")
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn with_model(&self, _model: Option<String>) -> Box<dyn AgentProvider> {
        Box::new(self.clone())
    }
}

struct DeterministicAgentSession {
    exit_success: bool,
    session_ref: Option<AgentSessionRef>,
}

#[async_trait::async_trait]
impl AgentSession for DeterministicAgentSession {
    fn try_wait(&mut self) -> Result<Option<bool>> {
        Ok(Some(self.exit_success))
    }

    async fn wait(&mut self) -> Result<bool> {
        Ok(self.exit_success)
    }

    fn kill(&mut self) -> Result<()> {
        self.exit_success = false;
        Ok(())
    }

    fn pid(&self) -> Option<u32> {
        None
    }

    fn session_ref(&self) -> Option<AgentSessionRef> {
        self.session_ref.clone()
    }
}

fn apply_action(work_dir: &Path, action: &DeterministicAgentAction) -> Result<()> {
    match action {
        DeterministicAgentAction::WriteFile { file, contents } => {
            write_file(work_dir, file, contents)
        }
        DeterministicAgentAction::CommitWithBeadRef {
            bead_id,
            file,
            contents,
        } => {
            write_file(work_dir, file, contents)?;
            let message = format!("[{bead_id}] test(fake): scripted change\n\nbead:{bead_id}");
            git_commit_all(work_dir, &message)
        }
        DeterministicAgentAction::CommitPlain {
            message,
            file,
            contents,
        } => {
            write_file(work_dir, file, contents)?;
            git_commit_all(work_dir, message)
        }
    }
}

fn write_file(work_dir: &Path, file: &Path, contents: &str) -> Result<()> {
    let path = work_dir.join(file);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))
}

fn git_commit_all(work_dir: &Path, message: &str) -> Result<()> {
    let add = std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(work_dir)
        .output()
        .context("running git add for deterministic agent")?;
    if !add.status.success() {
        anyhow::bail!(
            "git add failed: {}",
            String::from_utf8_lossy(&add.stderr).trim()
        );
    }

    let commit = std::process::Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(work_dir)
        .output()
        .context("running git commit for deterministic agent")?;
    if !commit.status.success() {
        anyhow::bail!(
            "git commit failed: {}",
            String::from_utf8_lossy(&commit.stderr).trim()
        );
    }
    Ok(())
}
