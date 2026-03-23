//! Agent session abstractions — trait + CLI/Compute impls.
//!
//! `AgentSession` decouples from `tokio::process::Child` so we can support
//! CLI subprocesses, ACP sockets, raw API calls, container-dispatched agents, etc.

use anyhow::Result;

/// Abstract session to a running agent. Decouples from tokio::process::Child
/// so we can support CLI subprocesses, ACP sockets, raw API calls, etc.
#[async_trait::async_trait]
pub trait AgentSession: Send + Sync {
    /// Non-blocking check: has the session completed? Returns true on success.
    fn try_wait(&mut self) -> Result<Option<bool>>;

    /// Block until the session completes. Returns true on success.
    async fn wait(&mut self) -> Result<bool>;

    /// Forcefully terminate the session.
    fn kill(&mut self) -> Result<()>;

    /// Process ID (if applicable). For logging/debugging.
    #[allow(dead_code)] // Used by reconciler path, not MCP
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

/// Session for a container-dispatched agent. Currently exec() runs synchronously
/// in spawn() — the session is already resolved when returned. Non-blocking
/// background exec requires ComputeProvider: 'static + Clone (future work).
pub(crate) struct ComputeSession {
    pub(crate) rx: Option<tokio::sync::oneshot::Receiver<bool>>,
    pub(crate) result: Option<bool>,
}

#[async_trait::async_trait]
impl AgentSession for ComputeSession {
    fn try_wait(&mut self) -> Result<Option<bool>> {
        if let Some(result) = self.result {
            return Ok(Some(result));
        }
        if let Some(ref mut rx) = self.rx {
            match rx.try_recv() {
                Ok(success) => {
                    self.result = Some(success);
                    self.rx = None;
                    Ok(Some(success))
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => Ok(None),
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    // Sender dropped (task panicked) — treat as failure
                    self.result = Some(false);
                    self.rx = None;
                    Ok(Some(false))
                }
            }
        } else {
            Ok(self.result)
        }
    }
    async fn wait(&mut self) -> Result<bool> {
        if let Some(result) = self.result {
            return Ok(result);
        }
        if let Some(rx) = self.rx.take() {
            let success = rx.await.unwrap_or(false);
            self.result = Some(success);
            Ok(success)
        } else {
            Ok(false)
        }
    }
    fn kill(&mut self) -> Result<()> {
        // Drop the receiver — the background task will see a closed channel
        self.rx = None;
        self.result = Some(false);
        Ok(())
    }
}
