//! Thin binary: same listen path as `wirekern serve`. Key minting stays on
//! the CLI (`wirekern keys create`).

use clap::Parser;
use std::path::PathBuf;
use wirekern::{Error, WireError};

#[derive(Parser, Debug)]
#[command(
    name = "wirekern-serve",
    about = "Local HTTP for wirekern. Same JSON as wirekern --json. Runs until interrupt."
)]
struct Cli {
    #[arg(long, global = true)]
    json: bool,
    #[arg(long, global = true, env = "WIREKERN_HOME")]
    home: Option<PathBuf>,
    /// Default 127.0.0.1:8788.
    #[arg(long)]
    bind: Option<String>,
}

fn resolve_home(flag: Option<PathBuf>, user_home: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(p) = flag {
        return Ok(p);
    }
    user_home.map(|h| h.join(".wirekern")).ok_or_else(|| {
        "WIREKERN_HOME or HOME must be set to locate the vault; refusing to guess from the current directory".into()
    })
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let home = match resolve_home(cli.home, std::env::var_os("HOME").map(PathBuf::from)) {
        Ok(h) => h,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };
    if let Err(e) = wirekern_serve::run(&home, cli.bind.as_deref(), cli.json).await {
        emit_err(&e, cli.json);
        std::process::exit(e.exit_code());
    }
}

fn emit_err(e: &Error, json: bool) {
    if json {
        let w = WireError::from(e);
        println!("{}", serde_json::to_string(&w).expect("json"));
    } else {
        eprintln!("{e}");
    }
}
