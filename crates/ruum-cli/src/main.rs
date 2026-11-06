//! `ruum`: the command line client of Ruum. See the library (`ruum_cli`) for the commands.

use clap::Parser as _;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = ruum_cli::Cli::parse();
    let mut out = std::io::stdout().lock();
    match ruum_cli::run(cli, &mut out).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{}", ruum_cli::report(&e));
            std::process::ExitCode::FAILURE
        }
    }
}
