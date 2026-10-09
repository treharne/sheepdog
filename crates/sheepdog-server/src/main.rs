use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use sheepdog_server::Daemon;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Notify;

#[derive(Parser)]
#[command(about = "Sheepdog's per-user Ubuntu server and SSH bootstrap helper")]
struct Args {
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(long, default_value = "0.0.0.0:4443")]
        bind: SocketAddr,
        #[arg(long, default_value = "/bin/bash")]
        shell: PathBuf,
    },
    Bootstrap,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let directory = match args.state_dir {
        Some(path) => path,
        None => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?)
            .join(".local/state/sheepdog"),
    };
    match args.command {
        Command::Bootstrap => {
            let bootstrap = sheepdog_server::bootstrap(directory).await?;
            println!("{}", serde_json::to_string(&bootstrap)?);
        }
        Command::Serve { bind, shell } => {
            let daemon = Daemon::bind(bind, directory, shell)?;
            eprintln!(
                "Sheepdog listening on UDP {} (server user only)",
                daemon.port
            );
            let shutdown = Arc::new(Notify::new());
            let stop = shutdown.clone();
            tokio::spawn(async move {
                let mut term =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("SIGTERM handler");
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
                stop.notify_one();
            });
            daemon.serve(shutdown).await?;
        }
    }
    Ok(())
}
