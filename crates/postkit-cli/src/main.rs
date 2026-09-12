mod accounts;
mod ads;
mod app;
mod apps;
mod auth;
mod cli;
mod commands;
mod dispatch;
mod keys;
mod media;
mod output;
mod pages;
mod post;
mod whatsapp;

#[cfg(test)]
mod tests;

use crate::app::{fail, make_client, resolve_home};
use crate::cli::Cli;
use crate::commands::{whatsapp_send_allowed, Commands};
use crate::whatsapp::WhatsAppCmd;
use clap::Parser;
use postkit::Deadline;
use std::path::PathBuf;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli).await {
        std::process::exit(e);
    }
}

async fn run(cli: Cli) -> Result<(), i32> {
    let home =
        resolve_home(cli.home, std::env::var_os("HOME").map(PathBuf::from)).map_err(|msg| {
            eprintln!("{msg}");
            2
        })?;
    let json = cli.json;
    let account = cli.account.clone();
    let deadline = Deadline::from_secs(cli.deadline);

    match *cli.command {
        Commands::Apps(cmd) => apps::run(cmd, &home, json),
        Commands::Accounts(cmd) => accounts::run(cmd, &home, &account, json),
        Commands::Serve { bind } => postkit_serve::run(&home, bind.as_deref(), json)
            .await
            .map_err(|e| fail(&e, json)),
        Commands::Mcp => {
            // MCP stdio reserves stdout for JSON-RPC. A `--json` document
            // here would corrupt the host's protocol stream.
            if json {
                eprintln!("mcp uses stdout for JSON-RPC; omit --json");
                return Err(2);
            }
            postkit_mcp::run(&home).await.map_err(|e| fail(&e, false))
        }
        Commands::Keys(cmd) => keys::run(cmd, &home, json),
        Commands::WhatsApp(WhatsAppCmd::Configure {
            phone_number_id,
            waba_id,
            business_id,
            app_secret,
            verify_token,
            senders,
        }) => whatsapp::configure(
            phone_number_id,
            waba_id,
            business_id,
            app_secret,
            verify_token,
            senders,
            &home,
            json,
        ),
        other => {
            let allow_whatsapp_send = whatsapp_send_allowed(&other);
            let client = make_client(&home, allow_whatsapp_send).map_err(|e| fail(&e, json))?;
            let client = match &other {
                Commands::Ads(ads) => ads::apply_lifecycle_policy(client, ads),
                _ => client,
            };
            dispatch::dispatch(client, &home, other, json, account, deadline).await
        }
    }
}
