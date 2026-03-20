//! CLI subprocess session — wraps tokio::process::Child.

use anyhow::Result;

/// Abstract session to a running agent. Decouples from tokio::process::Child
/// so we can support CLI subprocesses, ACP sockets, raw API calls, etc.
#[async_trait::async_trait]
pub trait AgentSession: Send {
    /// Non-blocking check: has the session completed? Returns true on success.
    fn try_wait(&mut self) -> Result<Option<bool>>;

    /// Block until the session completes. Returns true on success.
    async fn wait(&mut self) -> Result<bool>;

    /// Forcefully terminate the session.
    fn kill(&mut self) -> Result<()>;

    /// Process ID (if applicable). For logging/debugging.
    fn pid(&self) -> Option<u32> {
        None
    }
}

/// CLI subprocess session — wraps tokio::process::Child.
pub struct CliSession {
    child: tokio::process::Child,
}

impl CliSession {
    pub fn new(child: tokio::process::Child) -> Self {
        Self { child }
    }
}

#[async_trait::async_trait]
impl AgentSession for CliSession {
    fn try_wait(&mut self) -> Result<Option<bool>> {
        match self.child.try_wait()? {
            Some(status) => Ok(Some(status.success())),
            None => Ok(None),
        }
    }

    async fn wait(&mut self) -> Result<bool> {
        let status = self.child.wait().await?;
        Ok(status.success())
    }

    fn kill(&mut self) -> Result<()> {
        self.child.start_kill()?;
        Ok(())
    }

    fn pid(&self) -> Option<u32> {
        self.child.id()
    }
}
