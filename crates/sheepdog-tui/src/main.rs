use clap::{Parser, ValueEnum};
use sheepdog_client::{Client, SshOptions, bootstrap_ssh};

#[derive(Clone, Copy, ValueEnum)]
enum Predict {
    Adaptive,
    Never,
}
#[derive(Parser)]
#[command(
    name = "sheepdog",
    about = "Prototype local TUI for one remote Sheepdog server"
)]
struct Args {
    /// SSH target, including user/config alias as appropriate.
    target: String,
    #[arg(long, default_value = "ssh")]
    ssh: String,
    /// Absolute remote state directory; otherwise the server's default.
    #[arg(long)]
    state_directory: Option<String>,
    /// Directly reachable QUIC address, when different from SSH HostName.
    #[arg(long)]
    quic_host: Option<String>,
    #[arg(long, value_enum, default_value = "adaptive")]
    predict: Predict,
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let (host, bootstrap) = bootstrap_ssh(&SshOptions {
        command: args.ssh,
        target: args.target.clone(),
        state_directory: args.state_directory,
        quic_host: args.quic_host,
    })
    .await?;
    let client = Client::connect(host, bootstrap).await?;
    sheepdog_tui::run(client, &args.target, matches!(args.predict, Predict::Never)).await
}
