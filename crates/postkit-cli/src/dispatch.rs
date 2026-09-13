//! Route a constructed `Client` to the matching CLI verb.
use crate::ads;
use crate::app::fail;
use crate::auth;
use crate::commands::Commands;
use crate::media;
use crate::output::{emit_raw, human_line};
use crate::pages;
use crate::post;
use crate::whatsapp;
use crate::x;
use postkit::{Client, Deadline, Error, Site};
use std::path::Path;

pub(crate) async fn dispatch(
    client: Client,
    home: &Path,
    cmd: Commands,
    json: bool,
    account: String,
    deadline: Deadline,
) -> Result<(), i32> {
    match cmd {
        Commands::WhatsApp(cmd) => whatsapp::dispatch(client, cmd, json, account, deadline).await,
        Commands::X(cmd) => x::dispatch(client, cmd, json, account, deadline).await,
        Commands::Ads(cmd) => ads::dispatch(client, home, cmd, json, account, deadline).await,
        Commands::Media(cmd) => media::dispatch(&client, cmd, json, &account, deadline).await,
        Commands::Pages(cmd) => pages::dispatch(&client, cmd, json, &account, deadline).await,
        Commands::Capabilities { site } => capabilities(&client, site, json),
        Commands::Whoami { site } => auth::whoami(&client, site, json, &account).await,
        Commands::Insights {
            site,
            from,
            until,
            level,
            metrics,
            attribution,
            ad_account,
            entity_ids,
            breakdowns,
            async_report,
            report,
        } => {
            ads::run_insights(
                &client,
                home,
                site,
                from,
                until,
                level,
                metrics,
                attribution,
                ad_account,
                entity_ids,
                breakdowns,
                async_report,
                report,
                json,
                &account,
                deadline,
            )
            .await
        }
        Commands::Auth {
            site,
            token,
            code,
            password,
            system_user,
            with_dm,
        } => {
            auth::run(
                &client,
                home,
                site,
                token,
                code,
                password,
                system_user,
                with_dm,
                json,
                account,
                deadline,
            )
            .await
        }
        Commands::Post {
            site,
            text,
            to,
            param,
            reply_to,
            idempotency,
            stdin,
            dry_run,
            image,
            alt,
            page_id,
        } => {
            post::run(
                &client,
                site,
                text,
                to,
                param,
                reply_to,
                page_id,
                idempotency,
                stdin,
                dry_run,
                image,
                alt,
                json,
                account,
                deadline,
            )
            .await
        }
        Commands::Apps(_)
        | Commands::Accounts(_)
        | Commands::Serve { .. }
        | Commands::Mcp
        | Commands::Keys(_) => unreachable!("handled in run"),
    }
}

fn capabilities(client: &Client, site: Option<String>, json: bool) -> Result<(), i32> {
    if let Some(s) = site {
        let site = Site::new(s);
        match client.registry().capabilities_for(&site) {
            Some(caps) => {
                let names: Vec<&str> = caps.iter().map(|c| c.as_str()).collect();
                if json {
                    emit_raw(&serde_json::json!({ site.as_str(): names }));
                } else {
                    human_line(format!("{}: {}", site, names.join(", ")));
                }
            }
            None => {
                return Err(fail(&Error::UnknownSite(site), json));
            }
        }
    } else if json {
        emit_raw(&client.registry().capabilities_json());
    } else {
        human_line(client.registry().capabilities_json().to_string());
    }
    Ok(())
}
