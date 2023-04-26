use std::default::Default;
use std::fmt;
use std::sync::{Arc, Weak};
use tokio::sync::{mpsc, watch};

/// A handle held by an active task, which can be used to receive
/// shutdown notifications and to delay program termination until the
/// task has completed.
pub struct ShutdownHandle {
    /// Receives cancellation notifications.
    cancellation_rx: ShutdownSignal,
    /// Signals connection completion (when dropped)
    _shutdown_tx: mpsc::Sender<()>,
}

impl Default for ShutdownHandle {
    fn default() -> Self {
        let (_shutdown_tx, _) = mpsc::channel(1);
        ShutdownHandle {
            cancellation_rx: ShutdownSignal::default(),
            _shutdown_tx,
        }
    }
}

impl fmt::Debug for ShutdownHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("ShutdownHandle").finish()
    }
}

/// Receives a shutdown signal, which can be awaited on once using the `on_shutdown` method.
#[derive(Clone)]
pub enum ShutdownSignal {
    WaitingForSignal(watch::Receiver<bool>),
    Signalled,
}

impl ShutdownSignal {
    /// Wait for a shutdown signal to happen once.
    /// This function can be called multiple times but it will only resolve once,
    /// so it can be used in a `select!` block to receive the shutdown signal once.
    pub async fn on_shutdown(&mut self) {
        match self {
            ShutdownSignal::WaitingForSignal(r) => {
                let _ = r.changed().await;
                *self = ShutdownSignal::Signalled;
            }
            ShutdownSignal::Signalled => {
                futures::future::pending::<()>().await;
            }
        }
    }
}

/// The default implementation of ShutdownSignal will never be signalled.
impl Default for ShutdownSignal {
    fn default() -> Self {
        ShutdownSignal::Signalled
    }
}

impl From<&ShutdownHandle> for ShutdownSignal {
    fn from(handle: &ShutdownHandle) -> Self {
        handle.cancellation_rx.clone()
    }
}

/// Coordinates the shutdown process for a group of tasks.
/// This allows the tasks to get notified when a shutdown is requested,
/// and allows the main thread to defer termination until all of the tasks have successfully
/// completed.
pub struct ShutdownCoordinator {
    /// Holds onto the ShutdownHandle until shutdown starts.
    shutdown_handle: Arc<ShutdownHandle>,
    /// Used to notify tasks to start shutdown
    cancellation_tx: watch::Sender<bool>,
    /// Used to wait for all connections to shutdown successfully.
    shutdown_rx: mpsc::Receiver<()>,
}

impl ShutdownCoordinator {
    /// Get a cancellation channel that, when dropped, can be used to close all proxy endpoints created from this object.
    pub fn new() -> Self {
        let (cancellation_tx, cancellation_rx) = watch::channel(false);
        let (shutdown_tx, shutdown_rx) = mpsc::channel(1);
        let shutdown_handle = Arc::new(ShutdownHandle {
            cancellation_rx: ShutdownSignal::WaitingForSignal(cancellation_rx),
            _shutdown_tx: shutdown_tx,
        });
        ShutdownCoordinator {
            shutdown_handle,
            cancellation_tx,
            shutdown_rx,
        }
    }

    /// Get a ShutdownHandle to be held by a task that needs to be waited on during shutdown.
    pub fn handle(&self) -> Arc<ShutdownHandle> {
        Arc::clone(&self.shutdown_handle)
    }

    /// Get a ShutdownHandle that can be held by a task that does not need to be waited on, but may
    /// spawn tasks that should be waited on. If the task can upgrade the handle with Arc::upgrade,
    /// then shutdown has not yet started.
    pub fn handle_weak(&self) -> Weak<ShutdownHandle> {
        Arc::downgrade(&self.shutdown_handle)
    }

    /// Initiate shutdown and wait for its successful completion.
    /// To prevent new connections from being accepted, drop any listening tasks first.
    pub async fn shutdown(mut self) {
        let _ = self.cancellation_tx.send(true);
        drop(self.shutdown_handle);
        let _ = self.shutdown_rx.recv().await;
    }

    /// Shutdown, waiting a maximum amount of time before returning.
    pub async fn shutdown_with_timeout(self, timeout: u64) {
        let _ =
            tokio::time::timeout(tokio::time::Duration::from_secs(timeout), self.shutdown()).await;
    }
}

impl Default for ShutdownCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{pin_mut, FutureExt};

    #[tokio::test]
    async fn test_shutdown_coordinator() {
        // Shutdown with no active tasks should happen immediately
        assert!(ShutdownCoordinator::new()
            .shutdown()
            .now_or_never()
            .is_some());

        // Shutdown is delayed with an active task
        let sc = ShutdownCoordinator::new();
        let handle = sc.handle();
        let shutdown_fut = sc.shutdown();
        pin_mut!(shutdown_fut);
        assert!(shutdown_fut.as_mut().now_or_never().is_none());
        drop(handle);
        assert!(shutdown_fut.now_or_never().is_some());
    }

    #[tokio::test]
    async fn test_default_shutdown_handle() {
        let handle = ShutdownHandle::default();
        let mut signal = ShutdownSignal::from(&handle);
        assert!(signal.on_shutdown().now_or_never().is_none());
    }
}
