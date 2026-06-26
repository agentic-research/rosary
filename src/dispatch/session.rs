//! Agent session abstractions — trait + CLI/Compute impls.
//!
//! `AgentSession` decouples from `tokio::process::Child` so we can support
//! CLI subprocesses, ACP sockets, raw API calls, container-dispatched agents, etc.

use anyhow::{Context, Result};

/// Provider-native handle for a session that may not have an OS process id.
///
/// Examples: a Codex thread id, an ACP session id, or a remote worker run id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionRef {
    pub provider: String,
    pub id: String,
}

impl AgentSessionRef {
    #[allow(dead_code)] // Public API for native providers; used by tests before Codex lands.
    pub fn new(provider: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            id: id.into(),
        }
    }
}

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

    /// Provider-native session identity for runtimes without an OS PID.
    fn session_ref(&self) -> Option<AgentSessionRef> {
        None
    }

    /// Drain and return tool call records accumulated during this session.
    /// Only ACP sessions produce non-empty results; all others return empty.
    /// `&mut self` makes the draining/consuming semantics explicit at call sites.
    fn take_tools_used(&mut self) -> Vec<crate::handoff::ToolCallRecord> {
        Vec::new()
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

/// Session wrapping a `std::process::Child`. Used by `PluginDispatchProvider`
/// which needs synchronous stdin writes that `tokio::process::Child` doesn't support.
///
/// `wait()` uses `block_in_place` — requires a multi-threaded tokio runtime,
/// which is the production default. Single-threaded test runtimes must use
/// `#[tokio::test(flavor = "multi_thread")]`.
pub(crate) struct StdCliSession {
    pub(crate) child: std::process::Child,
}

impl StdCliSession {
    #[allow(dead_code)] // Constructor for the std-process session backend.
    pub(crate) fn new(child: std::process::Child) -> Self {
        Self { child }
    }
}

#[async_trait::async_trait]
impl AgentSession for StdCliSession {
    fn try_wait(&mut self) -> Result<Option<bool>> {
        match self.child.try_wait()? {
            Some(status) => Ok(Some(status.success())),
            None => Ok(None),
        }
    }

    async fn wait(&mut self) -> Result<bool> {
        let status = tokio::task::block_in_place(|| self.child.wait())
            .context("waiting for dispatch plugin")?;
        Ok(status.success())
    }

    fn kill(&mut self) -> Result<()> {
        self.child.kill()?;
        Ok(())
    }

    fn pid(&self) -> Option<u32> {
        Some(self.child.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- ComputeSession state machine ---

    #[tokio::test]
    async fn compute_session_pending_returns_none() {
        let (_tx, rx) = tokio::sync::oneshot::channel::<bool>();
        let mut s = ComputeSession {
            rx: Some(rx),
            result: None,
        };
        assert!(s.try_wait().unwrap().is_none());
    }

    #[tokio::test]
    async fn compute_session_resolves_success() {
        let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
        let mut s = ComputeSession {
            rx: Some(rx),
            result: None,
        };
        tx.send(true).unwrap();
        // Give the channel a tick to deliver
        tokio::task::yield_now().await;
        assert_eq!(s.try_wait().unwrap(), Some(true));
        // Second call returns cached result
        assert_eq!(s.try_wait().unwrap(), Some(true));
    }

    #[tokio::test]
    async fn compute_session_resolves_failure() {
        let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
        let mut s = ComputeSession {
            rx: Some(rx),
            result: None,
        };
        tx.send(false).unwrap();
        tokio::task::yield_now().await;
        assert_eq!(s.try_wait().unwrap(), Some(false));
    }

    #[tokio::test]
    async fn compute_session_sender_dropped_yields_false() {
        let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
        drop(tx); // sender dropped without sending
        let mut s = ComputeSession {
            rx: Some(rx),
            result: None,
        };
        assert_eq!(s.try_wait().unwrap(), Some(false));
    }

    #[tokio::test]
    async fn compute_session_kill_sets_false() {
        let (_tx, rx) = tokio::sync::oneshot::channel::<bool>();
        let mut s = ComputeSession {
            rx: Some(rx),
            result: None,
        };
        s.kill().unwrap();
        assert_eq!(s.try_wait().unwrap(), Some(false));
    }

    #[tokio::test]
    async fn compute_session_wait_resolves() {
        let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
        let mut s = ComputeSession {
            rx: Some(rx),
            result: None,
        };
        tx.send(true).unwrap();
        assert!(s.wait().await.unwrap());
    }

    #[tokio::test]
    async fn compute_session_already_resolved_skips_rx() {
        let mut s = ComputeSession {
            rx: None,
            result: Some(true),
        };
        assert_eq!(s.try_wait().unwrap(), Some(true));
        assert!(s.wait().await.unwrap());
    }

    // --- AgentSession default: take_tools_used ---

    struct MinimalSession;

    #[async_trait::async_trait]
    impl AgentSession for MinimalSession {
        fn try_wait(&mut self) -> anyhow::Result<Option<bool>> {
            Ok(Some(true))
        }
        async fn wait(&mut self) -> anyhow::Result<bool> {
            Ok(true)
        }
        fn kill(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct NativeSession {
        session_ref: AgentSessionRef,
    }

    #[async_trait::async_trait]
    impl AgentSession for NativeSession {
        fn try_wait(&mut self) -> anyhow::Result<Option<bool>> {
            Ok(None)
        }
        async fn wait(&mut self) -> anyhow::Result<bool> {
            Ok(true)
        }
        fn kill(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
        fn session_ref(&self) -> Option<AgentSessionRef> {
            Some(self.session_ref.clone())
        }
    }

    #[test]
    fn native_session_can_expose_provider_ref_without_pid() {
        let session = NativeSession {
            session_ref: AgentSessionRef::new("codex", "thread-123"),
        };

        assert_eq!(session.pid(), None);
        assert_eq!(
            session.session_ref(),
            Some(AgentSessionRef::new("codex", "thread-123"))
        );
    }

    #[test]
    fn default_take_tools_used_is_empty() {
        let mut s = MinimalSession;
        assert!(s.take_tools_used().is_empty());
        // Idempotent — second call also empty
        assert!(s.take_tools_used().is_empty());
    }
}
