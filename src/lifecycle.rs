use super::ENV_HANDOVER_PIPE;
use crate::pipes::FdStringExt;
use async_trait::async_trait;
use std::env;
use std::io;
use std::pin::Pin;
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncWrite};

pub type PipeReader = Pin<Box<dyn AsyncRead + Send>>;
pub type PipeWriter = Pin<Box<dyn AsyncWrite + Send>>;

#[async_trait]
pub trait LifecycleHandler: Send {
    /// Called after the child process has been spawned, allowing the current process to send state
    /// to the child process. The child process can receive this data by calling
    /// `receive_from_old_process`.
    async fn send_to_new_process(&mut self, _write_pipe: PipeWriter) -> io::Result<()> {
        Ok(())
    }

    /// Called after `send_to_new_process` if the child process fails to start successfully.
    /// This gives you an opportunity to undo any state changes made in `send_to_new_process`.
    async fn new_process_failed(&mut self) {}
}

/// A default implementation of LifecycleHandler that does nothing in response to lifecycle events.
pub struct NullLifecycleHandler;

impl LifecycleHandler for NullLifecycleHandler {}

/// If this process has been spawned due to graceful restart, returns a `PipeReader` used to receive
/// data from the parent process's implementation of `LifecycleHandler::send_to_new_process`.
pub fn receive_from_old_process() -> Option<PipeReader> {
    if let Ok(handover_fd) = env::var(ENV_HANDOVER_PIPE) {
        File::from_fd_string(&handover_fd)
            .ok()
            .map(|x| Box::pin(x) as PipeReader)
    } else {
        None
    }
}
