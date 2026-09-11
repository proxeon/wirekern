//! Thin binary: same stdio path as `postkit mcp`. Vault home resolution
//! matches serve — never guess `./.postkit` from the current directory.

use clap::Parser;
use postkit::WireError;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "postkit-mcp",
    about = "Local MCP stdio for postkit. Stdout is JSON-RPC only. Runs until stdin closes."
)]
struct Cli {
    #[arg(long, global = true, env = "POSTKIT_HOME")]
    home: Option<PathBuf>,
}

fn resolve_home(flag: Option<PathBuf>, user_home: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(path) = flag {
        return Ok(path);
    }
    user_home.map(|home| home.join(".postkit")).ok_or_else(|| {
        "POSTKIT_HOME or HOME must be set to locate the vault; refusing to guess from the current directory".into()
    })
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let home = match resolve_home(cli.home, std::env::var_os("HOME").map(PathBuf::from)) {
        Ok(home) => home,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };
    if let Err(error) = postkit_mcp::run(&home).await {
        let wire = WireError::from(&error);
        eprintln!("{}", serde_json::to_string(&wire).expect("json"));
        std::process::exit(wire.exit_code());
    }
}
