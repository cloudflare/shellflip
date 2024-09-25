//! Graceful restart management inspired by tableflip, but more basic.
//!
//! To implement restarts, the simplest thing to do is to generate a `RestartConfig` from
//! command-line values or hardcoded defaults, then call `RestartConfig::try_into_restart_task`. If
//! you implement a restart command using unix sockets for interactive error reporting, call
//! `RestartConfig::request_restart` and return the Result in your main() function.
//!
//! The process is automatically placed into the ready state the first time the restart task is
//! polled. This should be put into a select statement with other futures your app may await on.
//! The restart task will resolve with `Ok(())` if a restart signal was sent and the new process
//! spawned successfully. If the task is unable to handle future restart signals for any reason,
//! it will resolve to an `Err`.
//!
//! The process can also be restarted by sending it SIGUSR1. After any kind of restart request, the
//! old process will terminate if the new process starts up successfully, otherwise it will
//! continue if possible.
//!
//! For coordinating graceful shutdown of the old process, see `ShutdownCoordinator` in the
//! `shutdown` module.
//!
//! # Restart thread
//!
//! Process restarts are handled by a dedicated thread which is spawned when calling either
//! `RestartConfig::try_into_restart_task` or `spawn_restart_task`. If you are dropping privileges,
//! capabilities or using seccomp policies to limit the syscalls that can execute, it is a good
//! idea to call the aforementioned functions before locking down the main & future child threads.
//! You likely don't want the restart thread to have the same restrictions and limitations that may
//! otherwise prevent you from calling execve() or doing certain I/O operations.
//!
//! # Transferring state to the new process
//!
//! It is possible for the old process to serialise state and send it to the new process to
//! continue processing. Your code must set an implementation of `LifecycleHandler` on the
//! `RestartConfig`. After a new process is spawned, shellflip will call
//! `LifecycleHandler::send_to_new_process` which gives you a unidirectional pipe to write data to
//! the new process, which receives this data by calling the `receive_from_old_process` function.
//!
//! The data should be received and validated by the new process before it signals readiness by
//! polling the restart task, in case the data is unusable e.g. if the data format changed slightly
//! between versions causing serialisation to fail. If the new process fails to signal readiness,
//! `LifecycleHandler::new_process_failed` is called and you can undo any changes you made in
//! preparation for handover. If the new process succeeds, however, the restart task will resolve
//! and you may terminate the process as usual.
pub mod lifecycle;
mod pipes;
pub mod restart_coordination_socket;
pub mod shutdown;

pub use shutdown::{ShutdownCoordinator, ShutdownHandle, ShutdownSignal};

use crate::lifecycle::LifecycleHandler;
use crate::pipes::{
    completion_pipes, create_paired_pipes, CompletionReceiver, CompletionSender, FdStringExt,
    PipeMode,
};
use crate::restart_coordination_socket::{
    RestartCoordinationSocket, RestartMessage, RestartRequest, RestartResponse,
};
use anyhow::anyhow;
use futures::stream::{Stream, StreamExt};
use std::env;
use std::ffi::OsString;
use std::fs::{remove_file, File as StdFile};
use std::future::Future;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::os::unix::net::UnixListener as StdUnixListener;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process;
use std::thread;
use thiserror::Error;
use tokio::fs::File;
use tokio::net::{UnixListener, UnixStream};
use tokio::select;
use tokio::signal::unix::{signal, Signal, SignalKind};
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio_stream::wrappers::UnixListenerStream;

pub type RestartResult<T> = anyhow::Result<T>;

const ENV_NOTIFY_SOCKET: &str = "OXY_NOTIFY_SOCKET";
const ENV_RESTART_SOCKET: &str = "OXY_RESTART_SOCKET";
const ENV_HANDOVER_PIPE: &str = "OXY_HANDOVER_PIPE";
const ENV_SYSTEMD_PID: &str = "LISTEN_PID";
const REBIND_SYSTEMD_PID: &str = "auto";

/// Settings for graceful restarts
pub struct RestartConfig {
    /// Enables the restart coordination socket for graceful restarts as an alternative to the SIGUSR1 signal.
    pub enabled: bool,
    /// Socket path
    pub coordination_socket_path: PathBuf,
    /// Sets environment variables on the newly-started process
    pub environment: Vec<(OsString, OsString)>,
    /// Receive fine-grained events on the lifecycle of the new process and support data transfer.
    pub lifecycle_handler: Box<dyn LifecycleHandler>,
    /// Exits early when child process fail to start
    pub exit_on_error: bool,
}

impl RestartConfig {
    /// Prepare the current process to handle restarts, if enabled.
    pub fn try_into_restart_task(
        self,
    ) -> io::Result<(impl Future<Output = RestartResult<process::Child>> + Send)> {
        fixup_systemd_env();
        spawn_restart_task(self)
    }

    /// Request an already-running service to restart.
    pub async fn request_restart(self) -> RestartResult<u32> {
        if !self.enabled {
            return Err(anyhow!(
                "no restart coordination socket socket defined in config"
            ));
        }

        let socket = UnixStream::connect(self.coordination_socket_path).await?;
        restart_coordination_socket::RestartCoordinationSocket::new(socket)
            .send_restart_command()
            .await
    }

    /// Request an already-running service to restart.
    /// Does not require the tokio runtime to be started yet.
    pub fn request_restart_sync(self) -> RestartResult<u32> {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(self.request_restart())
    }
}

impl Default for RestartConfig {
    fn default() -> Self {
        RestartConfig {
            enabled: false,
            coordination_socket_path: Default::default(),
            environment: vec![],
            lifecycle_handler: Box::new(lifecycle::NullLifecycleHandler),
            exit_on_error: true,
        }
    }
}

/// When the proxy restarts itself, it sets the child's LISTEN_PID env to a
/// special value so that the child can replace it with the real child PID.
/// Doing this is easier than reimplementing rust's process spawn code just so
/// we can call execvpe to replace the environment in the forked process.
///
/// This is usually called by `RestartConfig::try_into_restart_task` but this function is available
/// if it needs to be done at an earlier or more convenient time, such as the top of `fn main()`.
pub fn fixup_systemd_env() {
    #[cfg(target_os = "linux")]
    if let Ok(true) = env::var(ENV_SYSTEMD_PID).map(|p| p == REBIND_SYSTEMD_PID) {
        env::set_var(ENV_SYSTEMD_PID, process::id().to_string());
    }
}

/// Notify systemd and the parent process (if any) that the proxy has started successfully.
/// Returns an error if there was a parent process and we failed to notify it.
///
/// This is usually called by the restart task returned from `RestartConfig::try_into_restart_task`
/// but this function is available if indicating readiness needs to happen sooner or at a more
/// convenient time then first polling the restart task.
///
/// The behaviour of this function is undefined if the environment variables used by this crate to
/// pass file descriptor numbers were set by something other than shellflip spawning a new instance
/// of the calling process.
pub fn startup_complete() -> io::Result<()> {
    if let Ok(notify_fd) = env::var(ENV_NOTIFY_SOCKET) {
        pipes::CompletionSender(unsafe { std::fs::File::from_fd_string(&notify_fd)? }).send()?;
    }
    // Avoid sending twice on the notification pipe, if this is manually called outside
    // of the restart task.
    env::remove_var(ENV_NOTIFY_SOCKET);

    let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]);
    Ok(())
}

/// Returns the restart completion or error message through the restart coordination socket, if used.
struct RestartResponder {
    rpc: Option<RestartCoordinationSocket>,
}

impl RestartResponder {
    /// Send success or failure to the restart coordination socket client.
    async fn respond(self, result: Result<u32, String>) {
        let response = match result {
            Ok(pid) => RestartResponse::RestartComplete(pid),
            Err(e) => RestartResponse::RestartFailed(e),
        };
        if let Some(mut rpc) = self.rpc {
            if let Err(e) = rpc.send_message(RestartMessage::Response(response)).await {
                log::warn!("Failed to respond to restart coordinator: {}", e);
            }
        }
    }
}

/// Spawns a thread that can be used to restart the process.
/// Returns a future that resolves when a restart succeeds, or if restart
/// becomes impossible.
/// The child spawner thread needs to be created before seccomp locks down fork/exec.
pub fn spawn_restart_task(
    settings: RestartConfig,
) -> io::Result<impl Future<Output = RestartResult<process::Child>> + Send> {
    let socket = match settings.enabled {
        true => Some(settings.coordination_socket_path.as_ref()),
        false => None,
    };

    let mut signal_stream = signal(SignalKind::user_defined1())?;
    let (restart_fd, mut socket_stream) = new_restart_coordination_socket_stream(socket)?;
    let mut child_spawner =
        ChildSpawner::new(restart_fd, settings.environment, settings.lifecycle_handler);

    Ok(async move {
        startup_complete()?;
        loop {
            let responder = next_restart_request(&mut signal_stream, &mut socket_stream).await?;

            log::debug!("Spawning new process");
            let res = child_spawner.spawn_new_process().await;

            responder
                .respond(res.as_ref().map(|p| p.id()).map_err(|e| e.to_string()))
                .await;

            match res {
                Ok(child) => {
                    log::debug!("New process spawned with pid {}", child.id());

                    if let Err(e) =
                        sd_notify::notify(true, &[sd_notify::NotifyState::MainPid(child.id())])
                    {
                        log::error!("Failed to notify systemd: {}", e);
                    }

                    return Ok(child);
                }
                Err(ChildSpawnError::ChildError(e)) => {
                    if settings.exit_on_error {
                        return Err(anyhow!("Restart failed: {}", e));
                    } else {
                        log::error!("Restart failed: {}", e);
                    }
                }
                Err(ChildSpawnError::RestartThreadGone) => {
                    res?;
                }
            }
        }
    })
}

/// Handles forking a new client in a more privileged thread.
struct ChildSpawner {
    signal_sender: Sender<()>,
    pid_receiver: Receiver<io::Result<process::Child>>,
}

impl ChildSpawner {
    /// Create a ChildSpawner that will pass restart_fd to child processes.
    fn new(
        restart_fd: Option<OwnedFd>,
        environment: Vec<(OsString, OsString)>,
        mut lifecycle_handler: Box<dyn LifecycleHandler>,
    ) -> Self {
        let (signal_sender, mut signal_receiver) = channel(1);
        let (pid_sender, pid_receiver) = channel(1);

        thread::spawn(move || {
            let restart_fd = restart_fd.as_ref().map(OwnedFd::as_fd);

            while let Some(()) = signal_receiver.blocking_recv() {
                let child = tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(spawn_child(
                        restart_fd,
                        &environment,
                        &mut *lifecycle_handler,
                    ));

                pid_sender
                    .blocking_send(child)
                    .expect("parent needs to receive the child");
            }
        });

        ChildSpawner {
            signal_sender,
            pid_receiver,
        }
    }

    /// Spawn a process via IPC to the privileged thread.
    /// Returns the child pid on success.
    async fn spawn_new_process(&mut self) -> Result<process::Child, ChildSpawnError> {
        self.signal_sender
            .send(())
            .await
            .map_err(|_| ChildSpawnError::RestartThreadGone)?;
        match self.pid_receiver.recv().await {
            Some(Ok(child)) => Ok(child),
            Some(Err(e)) => Err(ChildSpawnError::ChildError(e)),
            None => Err(ChildSpawnError::RestartThreadGone),
        }
    }
}

/// Indicates an error that happened during child forking.
#[derive(Error, Debug)]
pub enum ChildSpawnError {
    #[error("Restart thread exited")]
    RestartThreadGone,
    #[error("Child failed to start: {0}")]
    ChildError(io::Error),
}

/// Await the next request to gracefully restart the process.
/// Returns a RestartResponder used to receive the outcome of the restart attempt.
async fn next_restart_request(
    signal_stream: &mut Signal,
    mut socket_stream: impl Stream<Item = RestartResponder> + Unpin,
) -> RestartResult<RestartResponder> {
    select! {
        _ = signal_stream.recv() => Ok(RestartResponder{ rpc: None }),
        r = socket_stream.next() => match r {
            Some(r) => Ok(r),
            None => {
                // Technically we can still support signal restart! However if you have the restart coordination
                // socket enabled you probably don't want to use signals, and need to recover the process such
                // that you can use the restart coordinator socket again.
                Err(anyhow!("Restart coordinator socket acceptor terminated"))
            }
        }
    }
}

fn new_restart_coordination_socket_stream(
    restart_coordination_socket: Option<&Path>,
) -> io::Result<(Option<OwnedFd>, impl Stream<Item = RestartResponder>)> {
    if let Some(path) = restart_coordination_socket {
        let listener = bind_restart_coordination_socket(path)?;
        let inherit_socket = OwnedFd::from(listener.try_clone()?);
        let listener = UnixListener::from_std(listener)?;
        let st = listen_for_restart_events(listener);
        Ok((Some(inherit_socket), st.boxed()))
    } else {
        Ok((None, futures::stream::pending().boxed()))
    }
}

fn bind_restart_coordination_socket(path: &Path) -> io::Result<StdUnixListener> {
    match env::var(ENV_RESTART_SOCKET) {
        Err(_) => {
            // This may fail but binding will succeed despite that. If binding fails,
            // that's the error we really care about.
            let _ = remove_file(path);
            StdUnixListener::bind(path)
        }
        Ok(maybe_sock_fd) => unsafe { StdUnixListener::from_fd_string(&maybe_sock_fd) },
    }
}

fn listen_for_restart_events(
    restart_coordination_socket: UnixListener,
) -> impl Stream<Item = RestartResponder> {
    UnixListenerStream::new(restart_coordination_socket).filter_map(move |r| async move {
        let sock = match r {
            Ok(sock) => sock,
            Err(e) => {
                log::error!("Restart coordination socket accept error: {}", e);
                return None;
            }
        };

        let mut rpc = RestartCoordinationSocket::new(sock);
        match rpc.receive_message().await {
            Ok(RestartMessage::Request(RestartRequest::TryRestart)) => {
                Some(RestartResponder { rpc: Some(rpc) })
            }
            Ok(m) => {
                log::warn!(
                    "Restart coordination socket received unexpected message: {:?}",
                    m
                );
                None
            }
            Err(e) => {
                log::warn!("Restart coordination socket connection error: {}", e);
                None
            }
        }
    })
}

/// Clears the FD_CLOEXEC flag on a fd so it can be inherited by a child process.
fn clear_cloexec(fd: RawFd) -> nix::Result<()> {
    use nix::fcntl::*;
    let mut current_flags = FdFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFD)?);
    current_flags.remove(FdFlag::FD_CLOEXEC);
    fcntl(fd, FcntlArg::F_SETFD(current_flags))?;
    Ok(())
}

/// Attempt to start a new instance of this proxy.
async fn spawn_child(
    restart_fd: Option<BorrowedFd<'_>>,
    user_envs: &[(OsString, OsString)],
    lifecycle_handler: &mut dyn LifecycleHandler,
) -> io::Result<process::Child> {
    lifecycle_handler.pre_new_process().await;

    let mut args = env::args();
    let process_name = args.next().unwrap();

    // Create a pipe for the child to notify us on successful startup
    let (notif_r, notif_w) = completion_pipes()?;

    // And another pair of pipes to hand over data to the child process.
    let (handover_r, handover_w) = create_paired_pipes(PipeMode::ParentWrites)?;

    let mut cmd = process::Command::new(process_name);
    cmd.args(args)
        .envs(user_envs.iter().map(|(k, v)| (k, v)))
        .env(ENV_SYSTEMD_PID, REBIND_SYSTEMD_PID)
        .env(ENV_HANDOVER_PIPE, handover_r.fd_string())
        .env(ENV_NOTIFY_SOCKET, notif_w.0.fd_string());

    if let Some(fd) = restart_fd {
        // Let the child inherit the restart coordination socket
        let fd = fd.as_raw_fd();
        unsafe {
            cmd.env(ENV_RESTART_SOCKET, fd.to_string())
                .pre_exec(move || {
                    clear_cloexec(fd)?;
                    Ok(())
                });
        }
    }
    let mut child = cmd.spawn()?;

    if let Err(e) = send_parent_state(lifecycle_handler, notif_r, notif_w, handover_w).await {
        if child.kill().is_err() {
            log::error!("Child process has already exited. Failed to send parent state: {e:?}");
        } else {
            log::error!("Killed child process because failed to send parent state: {e:?}");
        }
        return Err(e);
    }

    Ok(child)
}

async fn send_parent_state(
    lifecycle_handler: &mut dyn LifecycleHandler,
    mut notif_r: CompletionReceiver,
    notif_w: CompletionSender,
    handover_w: StdFile,
) -> io::Result<()> {
    lifecycle_handler
        .send_to_new_process(Box::pin(File::from(handover_w)))
        .await?;

    // only the child needs the write end
    drop(notif_w);
    match notif_r.recv() {
        Ok(_) => Ok(()),
        Err(e) => {
            lifecycle_handler.new_process_failed().await;
            Err(e)
        }
    }
}
