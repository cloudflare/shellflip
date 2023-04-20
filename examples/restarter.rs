//! Sample restarter application.
//! This implements a TCP server that accepts connections,
//! outputs a short line describing the running process,
//! then echoes back anything sent to it by the client.
//!
//! While the application is running, another instance can be invoked with the
//! `restart` command which will trigger a restart. Existing connections will be maintained and the
//! old process will terminate as soon as all clients disconnect. The new process will listen on
//! another socket (as this library does not provide for socket inheritance or rebinding).
use anyhow::Error;
use clap::{Parser, Subcommand};
use shellflip::{RestartConfig, ShutdownCoordinator, ShutdownHandle, ShutdownSignal};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::{pin, select};

/// Simple program to test graceful shutdown and restart
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Option<Commands>,
    /// Restart coordination socket path
    #[arg(short, long, default_value = "/tmp/restarter.sock")]
    socket: String,
}

#[derive(Subcommand)]
enum Commands {
    /// Trigger restart
    Restart,
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    env_logger::init();
    let args = Args::parse();

    // Configure the essential requirements for implementing graceful restart.
    let restart_conf = RestartConfig {
        enabled: true,
        coordination_socket_path: args.socket.into(),
    };

    match args.command {
        // Restart an already-running process
        Some(Commands::Restart) => {
            let res = restart_conf.request_restart().await;
            match res {
                Ok(id) => {
                    log::info!("Restart succeeded, child pid is {}", id);
                    return Ok(());
                }
                Err(e) => {
                    log::error!("Restart failed: {}", e);
                    return Err(e);
                }
            }
        }
        // Standard operating mode
        None => {}
    }

    // Start the restart thread and get a task that will complete when a restart completes.
    let restart_task = restart_conf.try_into_restart_task()?;
    // (need to pin this because of the loop below!)
    pin!(restart_task);
    // Create a shutdown coordinator so that we can wait for all client connections to complete.
    let shutdown_coordinator = ShutdownCoordinator::new();
    // Bind a TCP listener socket to give us something to do
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    println!("Listening on {}", listener.local_addr().unwrap());

    loop {
        select! {
            res = listener.accept() => {
                match res {
                    Ok((sock, addr)) => {
                        log::info!("Received connection from {}", addr);
                        // Spawn a new task to handle the client connection.
                        // Give it a shutdown handle so we can await its completion.
                        tokio::spawn(echo(sock, shutdown_coordinator.handle()));
                    }
                    Err(e) => {
                        log::warn!("Accept error: {}", e);
                    }
                }
            }
            res = &mut restart_task => {
                match res {
                    Ok(_) => {
                        log::info!("Restart successful, waiting for tasks to complete");
                    }
                    Err(e) => {
                        log::error!("Restart task failed: {}", e);
                    }
                }
                // Wait for all clients to complete.
                shutdown_coordinator.shutdown().await;
                log::info!("Exiting...");
                return Ok(());
            }
        }
    }
}

async fn echo(mut sock: TcpStream, shutdown_handle: Arc<ShutdownHandle>) {
    // Get notification that shutdown has been requested.
    // Note that we still keep the shutdown_handle active during the lifetime of this task.
    let mut shutdown_signal = ShutdownSignal::from(&*shutdown_handle);
    let mut buf = [0u8; 1024];
    let out = format!("Hello, this is process {}\n", std::process::id());
    let _ = sock.write_all(out.as_bytes()).await;

    loop {
        select! {
            r = sock.read(&mut buf) => {
                match r {
                    Ok(0) => return,
                    Ok(n) => {
                        if let Err(e) = sock.write_all(&buf[..n]).await {
                            log::error!("write failed: {}", e);
                            return;
                        }
                    }
                    Err(e) => {
                        log::error!("read failed: {}", e);
                        return;
                    }
                }
            }
            _ = shutdown_signal.on_shutdown() => {
                log::info!("shutdown requested but client {} is still active", sock.peer_addr().unwrap());
            }
        }
    }
}
