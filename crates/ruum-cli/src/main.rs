//! `ruum`: the command line client of Ruum. It talks to the REST API with an agent API key or a
//! user token. Owned by the MCP server + CLI area. This is a stub.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ruum", version, about = "The Ruum command line client")]
struct Cli {
    /// The base URL of the Ruum API.
    #[arg(long, env = "RUUM_URL", default_value = "https://ruum.localhost")]
    url: String,
    /// An agent API key (`ruum_agent_...`).
    #[arg(long, env = "RUUM_API_KEY", hide_env_values = true)]
    api_key: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show the signed-in principal.
    Whoami,
    /// List a folder.
    Ls { path: Option<String> },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Whoami | Command::Ls { .. } => {
            let _ = (&cli.url, &cli.api_key);
            anyhow::bail!("not implemented yet")
        }
    }
}
