//! Thin binary: same stdio path as `wirekern mcp`. Vault home resolution
//! matches serve — never guess `./.wirekern` from the current directory.

use clap::Parser;
use std::path::PathBuf;
use wirekern::WireError;

#[derive(Parser, Debug)]
#[command(
    name = "wirekern-mcp",
    about = "Local MCP stdio for wirekern. Stdout is JSON-RPC only. Runs until stdin closes."
)]
struct Cli {
    #[arg(long, global = true, env = "WIREKERN_HOME")]
    home: Option<PathBuf>,
}

fn resolve_home(flag: Option<PathBuf>, user_home: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(path) = flag {
        return Ok(path);
    }
    user_home.map(|home| home.join(".wirekern")).ok_or_else(|| {
        "WIREKERN_HOME or HOME must be set to locate the vault; refusing to guess from the current directory".into()
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
    if let Err(error) = wirekern_mcp::run(&home).await {
        let wire = WireError::from(&error);
        eprintln!("{}", serde_json::to_string(&wire).expect("json"));
        std::process::exit(wire.exit_code());
    }
}
