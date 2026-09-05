use clap::Parser;

use grammatic::cli::{Cli, run};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(if cli.debug {
            tracing::Level::DEBUG
        } else {
            tracing::Level::INFO
        })
        .init();

    match run(cli).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
