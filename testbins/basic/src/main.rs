//! Integration test binary for verifying process lifecycle.
use anyhow::{bail, Error};
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use futures::future::{Either, TryFutureExt};
use shellflip::lifecycle::*;
use shellflip::RestartConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const SERIALIZED_MAGIC_NUMBER: u32 = 0xCAFEF00D;

/// Simple program to test graceful shutdown and restart
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Option<Commands>,
    /// Restart coordination socket path
    #[arg(short, long)]
    socket: Option<String>,
    /// Use systemd restart lifecycle
    #[arg(long)]
    systemd: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Trigger restart
    Restart,
}

struct AppData {
    restart_generation: u32,
}

#[async_trait]
impl LifecycleHandler for AppData {
    async fn send_to_new_process(&mut self, mut write_pipe: PipeWriter<'_>) -> std::io::Result<()> {
        // Magic number
        write_pipe.write_u32(SERIALIZED_MAGIC_NUMBER).await?;

        if self.restart_generation > 4 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "The operation completed successfully",
            ));
        }
        write_pipe.write_u32(self.restart_generation).await?;
        Ok(())
    }
}

/// Restart an already-running process
async fn do_restart(restart_conf: RestartConfig) -> Result<(), Error> {
    match restart_conf.request_restart().await {
        Ok(_) => {
            println!("Restart request succeeded");
            Ok(())
        }
        Err(e) => {
            println!("Restart request failed: {}", e);
            Err(e)
        }
    }
}

/// Standard operating mode
async fn do_main(restart_conf: RestartConfig, systemd: bool) -> Result<(), Error> {
    let mut app_data = AppData {
        restart_generation: 0,
    };

    if let Some(mut handover_pipe) = receive_from_old_process() {
        if handover_pipe.read_u32().await? != SERIALIZED_MAGIC_NUMBER {
            bail!("Expected serialized data to begin with the magic number");
        }

        app_data.restart_generation = handover_pipe.read_u32().await? + 1;
    }

    let generation = app_data.restart_generation;
    // Start the restart thread and get a task that will complete when a restart completes.
    let restart_task = if systemd {
        Either::Left(restart_conf.try_into_systemd_restart_task(app_data)?)
    } else {
        Either::Right(restart_conf.try_into_restart_task(app_data)?.map_ok(|_| ()))
    };
    // Bind a TCP listener socket to give us something to do
    println!("Started with generation {}", generation);

    match restart_task.await {
        Ok(_) => {
            println!("Restart successful");
            Ok(())
        }
        Err(e) => {
            println!("Restart task failed: {}", e);
            Err(e)
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let args = Args::parse();

    // Configure the essential requirements for implementing graceful restart.
    let restart_conf = RestartConfig {
        enabled: true,
        coordination_socket_path: args.socket.unwrap_or_default().into(),
        ..Default::default()
    };

    match args.command {
        Some(Commands::Restart) => do_restart(restart_conf).await,
        None => do_main(restart_conf, args.systemd).await,
    }
}
