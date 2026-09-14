//! Production-facing Threads review app entry point.

use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;
use wirekern::Error;

#[derive(Parser, Debug)]
#[command(
    name = "wirekern-threads-app",
    about = "Small reviewable Threads scheduling app built on Wirekern."
)]
struct Cli {
    /// Public HTTPS origin, e.g. https://threads.example.com. Its callback is
    /// always /auth/threads/callback and must exactly match Meta's setting.
    #[arg(long, env = "WIREKERN_THREADS_APP_PUBLIC_URL")]
    public_url: String,
    /// Owner-only persistent application data. Do not place this in the repo.
    #[arg(long, env = "WIREKERN_THREADS_APP_DATA_DIR")]
    data_dir: PathBuf,
    /// Plain HTTP listener for a local reverse proxy. The public endpoint
    /// must terminate HTTPS before it reaches users or Meta.
    #[arg(long, default_value = "127.0.0.1:8790")]
    bind: SocketAddr,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(error) =
        wirekern_serve::threads_app::run_threads_app(cli.data_dir, cli.public_url, cli.bind).await
    {
        emit_error(&error);
        std::process::exit(error.exit_code());
    }
}

fn emit_error(error: &Error) {
    // Error display deliberately avoids access tokens and client secrets.
    eprintln!("{error}");
}
