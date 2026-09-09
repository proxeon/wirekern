mod output;

use clap::{Parser, Subcommand};
use output::{emit_err, emit_ok, emit_raw, human_line};
use postkit::connectors::threads::validate_text;
use postkit::{
    app_source, extract_code, valid_name, verify_state, AccountKey, AdAccount, AppConfig, AppStore,
    AttributionWindow, AuthReply, BidStrategy, Body, Breakdown, CampaignObjective, Client,
    CreatePausedAdRequest, CreatedAd, DateRange, Deadline, Error, FileAppStore, FileVault,
    InsightRow, InsightsLevel, InsightsQuery, Intent, Metric, OAuthApp, PausedAd, PausedAdCreate,
    PausedAdset, PausedCampaign, PostRequest, Registry, Site, Vault,
};
use std::io::{self, BufRead, IsTerminal, Read};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(
    name = "postkit",
    version,
    about = "Publish to official APIs. BYO credentials."
)]
struct Cli {
    /// JSON document on stdout (agents). Human text on stderr otherwise.
    #[arg(long, global = true)]
    json: bool,
    /// Vault root. Default ~/.postkit
    #[arg(long, global = true, env = "POSTKIT_HOME")]
    home: Option<PathBuf>,
    /// Seconds for a network operation. Default 30.
    #[arg(long, global = true, default_value_t = 30)]
    deadline: u64,
    /// Account alias. Default default.
    #[arg(long, global = true, default_value = "default")]
    account: String,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Post {
        site: Option<String>,
        /// Repeatable. Two or more on threads = reply chain.
        #[arg(long, action = clap::ArgAction::Append)]
        text: Vec<String>,
        /// Comma-separated sites. Same text and --param on each.
        #[arg(long)]
        to: Option<String>,
        /// target extras, e.g. chat_id=-100
        #[arg(long = "param", value_name = "k=v")]
        param: Vec<String>,
        #[arg(long)]
        idempotency: Option<String>,
        /// Raw request JSON on stdin.
        #[arg(long)]
        stdin: bool,
        /// Create-only probe (threads): validate everything a publish
        /// would, publish nothing. The container expires in 24h.
        #[arg(long)]
        dry_run: bool,
    },
    Auth {
        site: String,
        #[arg(long)]
        token: Option<String>,
        #[arg(long)]
        code: Option<String>,
        #[arg(long)]
        listen: bool,
        /// Bluesky app password. Omit the value to prompt on a TTY.
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        password: Option<String>,
    },
    Whoami {
        site: String,
    },
    /// Read spend/performance metrics (meta_ads). Range ≤ 90 days.
    Insights {
        site: String,
        /// Range start, YYYY-MM-DD.
        #[arg(long)]
        from: String,
        /// Range end, YYYY-MM-DD (inclusive).
        #[arg(long)]
        to: String,
        /// account | campaign | adset | ad.
        #[arg(long, default_value = "account")]
        level: String,
        /// Comma-separated: spend,impressions,clicks,reach,ctr,cpc,cpm,purchases,purchase_value,roas.
        #[arg(long, default_value = "spend,impressions,clicks,purchases")]
        metrics: String,
        /// 7d_click_1d_view | 1d_click | 1d_view. Explicit — ROAS answers
        /// change with the window, so there is no default.
        #[arg(long)]
        attribution: String,
        /// Override the stored ad account (123 or act_123).
        #[arg(long)]
        ad_account: Option<String>,
        /// Repeatable campaign, ad set, or ad ID filter. Not valid at account level.
        #[arg(long = "entity-id")]
        entity_ids: Vec<String>,
        /// Comma-separated: country,publisher_platform,age.
        #[arg(long, default_value = "")]
        breakdowns: String,
    },
    /// Read-only advertising-account discovery (not local vault aliases).
    #[command(subcommand)]
    Ads(AdsCmd),
    Capabilities {
        site: Option<String>,
    },
    #[command(subcommand)]
    Accounts(AccountsCmd),
    #[command(subcommand)]
    Apps(AppsCmd),
}

#[derive(Subcommand, Debug)]
enum AccountsCmd {
    List {
        #[arg(long)]
        site: Option<String>,
    },
    Delete {
        site: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
enum AdsCmd {
    /// List Meta ad accounts visible to the selected credential.
    Accounts { site: String },
    /// Create a Meta campaign with status hard-coded to PAUSED.
    CreateCampaign {
        site: String,
        /// Override the account stored by Meta OAuth (123 or act_123).
        #[arg(long)]
        ad_account: Option<String>,
        #[arg(long)]
        name: String,
        /// awareness | traffic | engagement | leads | app_promotion | sales.
        #[arg(long)]
        objective: String,
        /// Comma-separated Meta special-ad categories; blank means none.
        #[arg(long, default_value = "")]
        special_ad_categories: String,
    },
    /// Create a Meta ad set with status hard-coded to PAUSED.
    CreateAdset {
        site: String,
        #[arg(long)]
        ad_account: Option<String>,
        #[arg(long)]
        name: String,
        #[arg(long)]
        campaign_id: String,
        /// Daily budget in the ad account's minor currency unit.
        #[arg(long)]
        daily_budget: u64,
        /// `lowest_cost_without_cap`; required so Meta cannot inherit a
        /// bid-cap or ROAS strategy whose constraint is absent.
        #[arg(long)]
        bid_strategy: String,
        #[arg(long)]
        billing_event: String,
        #[arg(long)]
        optimization_goal: String,
        /// JSON object with the Meta targeting specification.
        #[arg(long)]
        targeting_file: PathBuf,
    },
    /// Create a Meta ad with status hard-coded to PAUSED.
    CreateAd {
        site: String,
        #[arg(long)]
        ad_account: Option<String>,
        #[arg(long)]
        name: String,
        #[arg(long)]
        adset_id: String,
        /// Existing Meta ad-creative ID; postkit creates no creative defaults.
        #[arg(long)]
        creative_id: String,
    },
}

#[derive(Subcommand, Debug)]
enum AppsCmd {
    Show {
        site: String,
    },
    Set {
        site: String,
        #[arg(long)]
        client_id: String,
        #[arg(long)]
        client_secret: String,
        #[arg(long)]
        redirect_uri: String,
    },
}

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

    match cli.command {
        Commands::Apps(AppsCmd::Set {
            site,
            client_id,
            client_secret,
            redirect_uri,
        }) => {
            check_name(&site, json)?;
            let apps = FileAppStore::new(&home).map_err(|e| fail(&e, json))?;
            apps.put(&AppConfig {
                site: Site::new(&site),
                oauth: Some(OAuthApp {
                    client_id,
                    client_secret,
                    redirect_uri,
                }),
                extra: serde_json::json!({}),
            })
            .map_err(|e| fail(&e, json))?;
            // The env layer outranks the file; say so when the write that
            // just "succeeded" will never be read back while it is set.
            if app_source(&Site::new(&site)) == "env" {
                let key = site.to_ascii_uppercase().replace('-', "_");
                eprintln!(
                    "note: POSTKIT_{key}_CLIENT_ID/_CLIENT_SECRET are set and take precedence over apps/{site}.json"
                );
            }
            if json {
                emit_raw(&serde_json::json!({ "site": site }));
            } else {
                eprintln!("wrote {}/apps/{site}.json", home.display());
            }
            Ok(())
        }
        Commands::Apps(AppsCmd::Show { site }) => {
            check_name(&site, json)?;
            let apps = FileAppStore::new(&home).map_err(|e| fail(&e, json))?;
            let cfg = apps.get(&Site::new(&site)).map_err(|e| fail(&e, json))?;
            let id = cfg
                .oauth
                .as_ref()
                .map(|o| o.client_id.as_str())
                .unwrap_or("");
            let redir = cfg
                .oauth
                .as_ref()
                .map(|o| o.redirect_uri.as_str())
                .unwrap_or("");
            // env credentials silently outrank the file; the operator must
            // be able to see which layer answered
            let source = app_source(&Site::new(&site));
            if json {
                emit_raw(&serde_json::json!({
                    "site": site,
                    "source": source,
                    "client_id": id,
                    "redirect_uri": redir,
                }));
            } else {
                human_line(format!(
                    "site={site} source={source} client_id={id} redirect_uri={redir} client_secret=[redacted]"
                ));
            }
            Ok(())
        }
        Commands::Accounts(AccountsCmd::List { site }) => {
            let vault = FileVault::new(&home).map_err(|e| fail(&e, json))?;
            let filter = site.as_deref().map(Site::new);
            let keys = vault.list(filter.as_ref()).map_err(|e| fail(&e, json))?;
            if json {
                let rows: Vec<_> = keys
                    .iter()
                    .map(|k| serde_json::json!({ "site": k.site, "name": k.name }))
                    .collect();
                emit_raw(&serde_json::json!({ "accounts": rows }));
            } else {
                for k in keys {
                    human_line(format!("{}/{}", k.site, k.name));
                }
            }
            Ok(())
        }
        Commands::Accounts(AccountsCmd::Delete { site, yes }) => {
            if !yes {
                eprintln!("pass --yes to delete {site}/{account}");
                return Err(2);
            }
            let vault = FileVault::new(&home).map_err(|e| fail(&e, json))?;
            let key = AccountKey::new(&site, &account);
            vault.delete(&key).map_err(|e| fail(&e, json))?;
            if json {
                emit_raw(&serde_json::json!({ "deleted": { "site": site, "name": account } }));
            } else {
                eprintln!("deleted {site}/{account}");
            }
            Ok(())
        }
        other => {
            let client = make_client(&home).map_err(|e| fail(&e, json))?;
            dispatch(client, &home, other, json, account, deadline).await
        }
    }
}

async fn dispatch(
    client: Client,
    home: &std::path::Path,
    cmd: Commands,
    json: bool,
    account: String,
    deadline: Deadline,
) -> Result<(), i32> {
    match cmd {
        Commands::Ads(AdsCmd::Accounts { site }) => {
            let key = AccountKey::new(&site, &account);
            match client.ad_accounts(&key, deadline).await {
                Ok(reply) => {
                    if json {
                        emit_raw(&serde_json::to_value(&reply).expect("json"));
                    } else {
                        for ad_account in &reply.accounts {
                            human_line(ad_account_line(ad_account));
                        }
                    }
                    Ok(())
                }
                Err(e) => Err(fail(&e, json)),
            }
        }
        Commands::Ads(AdsCmd::CreateCampaign {
            site,
            ad_account,
            name,
            objective,
            special_ad_categories,
        }) => {
            let request = build_paused_campaign_request(
                &site,
                ad_account,
                &name,
                &objective,
                &special_ad_categories,
            )
            .map_err(|e| fail(&e, json))?;
            one_paused_create(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        Commands::Ads(AdsCmd::CreateAdset {
            site,
            ad_account,
            name,
            campaign_id,
            daily_budget,
            bid_strategy,
            billing_event,
            optimization_goal,
            targeting_file,
        }) => {
            // A file avoids shell-escaping a nested targeting object and
            // makes the exact audience specification reviewable before any
            // write. Do not put the file path in an error: CI paths and home
            // directories add no actionable operator information.
            let targeting = std::fs::read_to_string(targeting_file).map_err(|_| {
                fail(
                    &Error::InvalidQuery {
                        site: Site::new(&site),
                        reason: "targeting_file_unreadable".into(),
                    },
                    json,
                )
            })?;
            let request = build_paused_adset_request(
                &site,
                PausedAdsetOptions {
                    ad_account,
                    name,
                    campaign_id,
                    daily_budget,
                    bid_strategy,
                    billing_event,
                    optimization_goal,
                    targeting,
                },
            )
            .map_err(|e| fail(&e, json))?;
            one_paused_create(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        Commands::Ads(AdsCmd::CreateAd {
            site,
            ad_account,
            name,
            adset_id,
            creative_id,
        }) => {
            let request =
                build_paused_ad_request(&site, ad_account, &name, &adset_id, &creative_id)
                    .map_err(|e| fail(&e, json))?;
            one_paused_create(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        Commands::Capabilities { site } => {
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
        Commands::Whoami { site } => {
            let key = AccountKey::new(&site, &account);
            match client.whoami(&key).await {
                Ok(w) => {
                    emit_ok(&w, json, || {
                        format!("{} {} {}", w.site, w.id, w.handle.as_deref().unwrap_or(""))
                    });
                    Ok(())
                }
                Err(e) => Err(fail(&e, json)),
            }
        }
        Commands::Insights {
            site,
            from,
            to,
            level,
            metrics,
            attribution,
            ad_account,
            entity_ids,
            breakdowns,
        } => {
            let query = build_insights_query(
                &site,
                &from,
                &to,
                &level,
                &metrics,
                &attribution,
                InsightsOptions {
                    ad_account,
                    entity_ids,
                    breakdowns,
                },
            )
            .map_err(|e| fail(&e, json))?;
            let key = AccountKey::new(&site, &account);
            match client.insights(&key, query, deadline).await {
                Ok(reply) => {
                    if json {
                        emit_raw(&serde_json::to_value(&reply).expect("json"));
                    } else {
                        human_line(format!(
                            "{} {} {}",
                            reply.site,
                            reply.account_id,
                            reply.currency.as_deref().unwrap_or("-")
                        ));
                        for row in &reply.rows {
                            human_line(insight_line(row));
                        }
                    }
                    Ok(())
                }
                Err(e) => Err(fail(&e, json)),
            }
        }
        Commands::Auth {
            site,
            token,
            code,
            listen,
            password,
        } => {
            if listen {
                eprintln!("--listen is not implemented in this scaffold; paste the code instead");
                return Err(2);
            }
            let key = AccountKey::new(&site, &account);
            let result = if let Some(password) = password {
                if token.is_some() || code.is_some() {
                    eprintln!("--password cannot be combined with --token or --code");
                    return Err(2);
                }
                let secret = if password.is_empty() {
                    if !io::stdin().is_terminal() {
                        eprintln!("then: postkit auth {site} --account <handle> --password <app-password>");
                        return Ok(());
                    }
                    // Echo suppressed: an app password is a full
                    // account-access credential and must not land in the
                    // terminal scrollback, capture panes or screen-shares.
                    rpassword::prompt_password("app password: ").map_err(|_| 5)?
                } else {
                    password
                };
                client
                    .auth_finish(
                        &key,
                        AuthReply::AppPassword {
                            identifier: account.clone(),
                            secret,
                            pds: None,
                        },
                    )
                    .await
            } else if let Some(token) = token {
                if code.is_some() {
                    eprintln!("--token and --code are mutually exclusive");
                    return Err(2);
                }
                client.put_token(&key, &token).await
            } else if let Some(code) = code {
                let code = extract_code(&code).map_err(|e| fail(&e, json))?;
                client.auth_finish(&key, AuthReply::Pasted { code }).await
            } else {
                match client.auth_start(&Site::new(&site)).await {
                    Ok(start) => match start {
                        postkit::AuthStart::Browser {
                            authorize_url,
                            state,
                        } => {
                            eprintln!("open: {authorize_url}");
                            if !io::stdin().is_terminal() {
                                eprintln!("then: postkit auth {site} --code <code>");
                                return Ok(());
                            }
                            eprintln!("paste the redirected URL or code, then Enter");
                            let mut line = String::new();
                            io::stdin().lock().read_line(&mut line).map_err(|_| 5)?;
                            verify_state(&state, &line).map_err(|e| fail(&e, json))?;
                            let code = extract_code(&line).map_err(|e| fail(&e, json))?;
                            client.auth_finish(&key, AuthReply::Pasted { code }).await
                        }
                        postkit::AuthStart::PasteInstructions { hint } => {
                            eprintln!("{hint}");
                            if !io::stdin().is_terminal() {
                                eprintln!("then: postkit auth {site} --account <handle> --password <app-password>");
                                return Ok(());
                            }
                            // Echo suppressed for the same reason as the
                            // --password prompt above.
                            let secret =
                                rpassword::prompt_password("app password: ").map_err(|_| 5)?;
                            client
                                .auth_finish(
                                    &key,
                                    AuthReply::AppPassword {
                                        identifier: account.clone(),
                                        secret,
                                        pds: None,
                                    },
                                )
                                .await
                        }
                        postkit::AuthStart::None => {
                            eprintln!("this site does not use OAuth; pass --token or --password");
                            return Ok(());
                        }
                    },
                    Err(e) => Err(e),
                }
            };
            match result {
                Ok(w) => {
                    emit_ok(&w, json, || {
                        // Say where credentials landed so a misdirected
                        // vault is visible immediately (issue 014).
                        format!(
                            "ok {} {} (vault: {})",
                            w.site,
                            w.handle.as_deref().unwrap_or(&w.id),
                            home.display()
                        )
                    });
                    Ok(())
                }
                Err(e) => Err(fail(&e, json)),
            }
        }
        Commands::Post {
            site,
            text,
            to,
            param,
            idempotency,
            stdin,
            dry_run,
        } => {
            if let Some(e) = dry_run_conflict(dry_run, idempotency.as_deref(), text.len()) {
                return Err(fail(&e, json));
            }
            if stdin {
                let mut buf = String::new();
                io::stdin().read_to_string(&mut buf).map_err(|_| 5)?;
                let req: PostRequest = serde_json::from_str(&buf).map_err(|e| {
                    fail(
                        &Error::InvalidPost {
                            site: Site::new(""),
                            reason: format!("json:{e}"),
                            limit: None,
                        },
                        json,
                    )
                })?;
                let (key, mut intent) = req.into_key_intent().map_err(|e| fail(&e, json))?;
                intent.idempotency_key = idempotency;
                // --stdin has no dry_run field of its own; the CLI flag is
                // the single switch, so both input paths stay in parity.
                if dry_run {
                    return one_probe(&client, &key, intent, deadline, json).await;
                }
                return one_post(&client, &key, intent, deadline, json).await;
            }
            let texts = resolve_texts(text)?;
            let params = parse_params(&param, json)?;
            let sites = collect_post_sites(site.as_deref(), to.as_deref())?;
            if texts.len() > 1 {
                if let Some(bad) = chain_blocked_site(&sites) {
                    return Err(fail(
                        &Error::InvalidPost {
                            site: Site::new(bad),
                            reason: "thread_unsupported".into(),
                            limit: None,
                        },
                        json,
                    ));
                }
                for t in &texts {
                    validate_text(t).map_err(|e| fail(&e, json))?;
                }
                return chain_threads(
                    &client,
                    &account,
                    &texts,
                    params,
                    idempotency,
                    deadline,
                    json,
                )
                .await;
            }
            let text = texts.into_iter().next().expect("resolve_texts");
            if let Some(to) = to {
                let mut results = Vec::new();
                let mut code = 0i32;
                for raw in to.split(',') {
                    let s = raw.trim();
                    if s.is_empty() {
                        continue;
                    }
                    let key = AccountKey::new(s, &account);
                    let intent = Intent {
                        site: Site::new(s),
                        params: params.clone(),
                        body: Body::Text { text: text.clone() },
                        idempotency_key: idempotency.clone(),
                    };
                    let attempt = if dry_run {
                        client
                            .probe(&key, intent, deadline)
                            .await
                            .map(|p| serde_json::to_value(&p).unwrap())
                    } else {
                        client
                            .publish(&key, intent, deadline)
                            .await
                            .map(|o| serde_json::to_value(&o).unwrap())
                    };
                    match attempt {
                        Ok(v) => results.push(v),
                        Err(e) => {
                            if code == 0 {
                                code = e.exit_code();
                            }
                            results
                                .push(serde_json::to_value(postkit::WireError::from(&e)).unwrap());
                        }
                    }
                }
                print_results(&results, json);
                if code == 0 {
                    Ok(())
                } else {
                    Err(code)
                }
            } else {
                let site = sites.into_iter().next().expect("collect_post_sites");
                let key = AccountKey::new(&site, &account);
                let intent = Intent {
                    site: Site::new(&site),
                    params,
                    body: Body::Text { text },
                    idempotency_key: idempotency,
                };
                if dry_run {
                    one_probe(&client, &key, intent, deadline, json).await
                } else {
                    one_post(&client, &key, intent, deadline, json).await
                }
            }
        }
        _ => unreachable!(),
    }
}

/// 027: `--dry-run` contradicts two other flags, and each contradiction
/// is refused loudly rather than half-honored — silently dropping part of
/// the operator's request is exactly the 022 failure mode. With
/// `--idempotency`: a probe neither consults nor records the ledger (see
/// `Client::probe`), so the pairing cannot mean anything. With a chain:
/// segment N+1 replies to segment N's *published* id, and a probe
/// publishes nothing — there is no parent to chain onto.
fn dry_run_conflict(dry_run: bool, idempotency: Option<&str>, texts: usize) -> Option<Error> {
    if !dry_run {
        return None;
    }
    if idempotency.is_some() {
        return Some(Error::InvalidPost {
            site: Site::new(""),
            reason: "dry_run_idempotency".into(),
            limit: None,
        });
    }
    if texts > 1 {
        return Some(Error::InvalidPost {
            site: Site::new("threads"),
            reason: "dry_run_chain".into(),
            limit: None,
        });
    }
    None
}

fn resolve_texts(text: Vec<String>) -> Result<Vec<String>, i32> {
    if text.is_empty() {
        eprintln!("--text is required (or --stdin)");
        return Err(2);
    }
    if text.iter().any(|t| t == "-") {
        if text.len() != 1 {
            eprintln!("--text - cannot be combined with other --text");
            return Err(2);
        }
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf).map_err(|_| 5)?;
        return Ok(vec![buf]);
    }
    Ok(text)
}

fn collect_post_sites(site: Option<&str>, to: Option<&str>) -> Result<Vec<String>, i32> {
    if let Some(to) = to {
        let sites: Vec<String> = to
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        if sites.is_empty() {
            eprintln!("site or --to is required");
            return Err(2);
        }
        Ok(sites)
    } else if let Some(site) = site {
        Ok(vec![site.to_string()])
    } else {
        eprintln!("site or --to is required");
        Err(2)
    }
}

fn chain_blocked_site(sites: &[String]) -> Option<&str> {
    sites.iter().map(String::as_str).find(|s| *s != "threads")
}

fn with_reply_to(params: &serde_json::Value, id: &str) -> serde_json::Value {
    let mut p = params.clone();
    match p {
        serde_json::Value::Object(ref mut m) => {
            m.insert(
                "reply_to_id".into(),
                serde_json::Value::String(id.to_string()),
            );
        }
        _ => {
            p = serde_json::json!({ "reply_to_id": id });
        }
    }
    p
}

async fn chain_threads(
    client: &Client,
    account: &str,
    texts: &[String],
    params: serde_json::Value,
    idempotency: Option<String>,
    deadline: Deadline,
    json: bool,
) -> Result<(), i32> {
    let key = AccountKey::new("threads", account);
    let mut results = Vec::new();
    let mut prev: Option<String> = None;
    let mut code = 0i32;
    for (i, text) in texts.iter().enumerate() {
        let p = if let Some(id) = prev.as_deref() {
            with_reply_to(&params, id)
        } else {
            params.clone()
        };
        let intent = Intent {
            site: Site::new("threads"),
            params: p,
            body: Body::Text { text: text.clone() },
            idempotency_key: if i == 0 { idempotency.clone() } else { None },
        };
        match client.publish(&key, intent, deadline).await {
            Ok(o) => {
                prev = o.id.clone();
                if prev.as_ref().map(|s| s.is_empty()).unwrap_or(true) {
                    let e = Error::Platform {
                        site: Site::new("threads"),
                        code: "missing_id".into(),
                        message: "Graph create returned no id".into(),
                    };
                    code = e.exit_code();
                    results.push(serde_json::to_value(postkit::WireError::from(&e)).unwrap());
                    break;
                }
                results.push(serde_json::to_value(&o).unwrap());
            }
            Err(e) => {
                code = e.exit_code();
                results.push(serde_json::to_value(postkit::WireError::from(&e)).unwrap());
                break;
            }
        }
    }
    print_results(&results, json);
    if code != 0 {
        if !json {
            eprintln!("published {} then failed", results.len().saturating_sub(1));
        }
        return Err(code);
    }
    Ok(())
}

/// Probe twin of `one_post`. The human line states the contract in plain
/// words — nothing was published — because a bare `site id` here would
/// read exactly like a successful post.
async fn one_probe(
    client: &Client,
    key: &AccountKey,
    intent: Intent,
    deadline: Deadline,
    json: bool,
) -> Result<(), i32> {
    match client.probe(key, intent, deadline).await {
        Ok(p) => {
            emit_ok(&p, json, || {
                format!(
                    "{} {} dry-run (nothing published, expires in {}h)",
                    p.site, p.container_id, p.expires_in_hours
                )
            });
            Ok(())
        }
        Err(e) => Err(fail(&e, json)),
    }
}

async fn one_post(
    client: &Client,
    key: &AccountKey,
    intent: Intent,
    deadline: Deadline,
    json: bool,
) -> Result<(), i32> {
    match client.publish(key, intent, deadline).await {
        Ok(o) => {
            emit_ok(&o, json, || {
                format!(
                    "{} {}",
                    o.id.as_deref().unwrap_or("-"),
                    o.url.as_deref().unwrap_or("")
                )
            });
            Ok(())
        }
        Err(e) => Err(fail(&e, json)),
    }
}

/// Paused ads have their own result type instead of being rendered as social
/// posts. The status is shown prominently so an operator can verify the
/// safety invariant in scripts and terminal output alike.
async fn one_paused_create(
    client: &Client,
    key: &AccountKey,
    request: CreatePausedAdRequest,
    deadline: Deadline,
    json: bool,
) -> Result<(), i32> {
    match client.create_paused_ad(key, request, deadline).await {
        Ok(created) => {
            emit_ok(&created, json, || created_ad_line(&created));
            Ok(())
        }
        Err(error) => Err(fail(&error, json)),
    }
}

/// `--json` prints `{ "results": [...] }`; human mode one line per result,
/// on stderr per the output-stream contract.
fn print_results(results: &[serde_json::Value], json: bool) {
    if json {
        emit_raw(&serde_json::json!({ "results": results }));
    } else {
        for r in results {
            human_line(result_line(r));
        }
    }
}

fn result_line(v: &serde_json::Value) -> String {
    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
        let site = v.get("site").and_then(|s| s.as_str()).unwrap_or("-");
        let reason = v.get("reason").and_then(|r| r.as_str()).unwrap_or("");
        return format!("{err} {site} {reason}").trim_end().into();
    }
    let site = v.get("site").and_then(|s| s.as_str()).unwrap_or("-");
    // probe rows carry container_id, not id — never render them like posts
    if let Some(c) = v.get("container_id").and_then(|i| i.as_str()) {
        return format!("{site} {c} dry-run");
    }
    let id = v.get("id").and_then(|i| i.as_str()).unwrap_or("-");
    let url = v.get("url").and_then(|u| u.as_str()).unwrap_or("");
    format!("{site} {id} {url}").trim_end().into()
}

fn parse_params(param: &[String], json: bool) -> Result<serde_json::Value, i32> {
    let mut map = serde_json::Map::new();
    for p in param {
        let Some((k, v)) = p.split_once('=') else {
            return Err(fail(
                &Error::InvalidPost {
                    site: Site::new(""),
                    reason: format!("param_not_k_eq_v:{p}"),
                    limit: None,
                },
                json,
            ));
        };
        map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
    }
    Ok(serde_json::Value::Object(map))
}

fn make_client(home: &std::path::Path) -> Result<Client, Error> {
    let mut registry = Registry::new();
    registry.register(Arc::new(postkit::connectors::threads::Threads::new()?));
    registry.register(Arc::new(postkit::connectors::bluesky::Bluesky::new()?));
    registry.register(Arc::new(postkit::connectors::meta_ads::MetaAds::new()?));
    let vault = Arc::new(FileVault::new(home)?);
    let apps = Arc::new(FileAppStore::new(home)?);
    Ok(Client::new(registry, vault, apps))
}

/// The optional, additive parts of an insights query. Keeping them together
/// prevents a growing CLI surface from turning the parser into a brittle,
/// positional argument list.
struct InsightsOptions {
    ad_account: Option<String>,
    entity_ids: Vec<String>,
    breakdowns: String,
}

/// Common builder error shape for advertising input. These errors name only
/// the invalid field, never echo targeting JSON or an operator's local path.
fn ads_input_error(site: &str, reason: impl Into<String>) -> Error {
    Error::InvalidQuery {
        site: Site::new(site),
        reason: reason.into(),
    }
}

fn build_paused_campaign_request(
    site: &str,
    ad_account: Option<String>,
    name: &str,
    objective: &str,
    special_ad_categories: &str,
) -> Result<CreatePausedAdRequest, Error> {
    let objective =
        CampaignObjective::from_str(objective).map_err(|reason| ads_input_error(site, reason))?;
    let request = CreatePausedAdRequest {
        account: ad_account,
        create: PausedAdCreate::Campaign(PausedCampaign {
            name: name.into(),
            objective,
            special_ad_categories: special_ad_categories
                .split(',')
                .map(str::trim)
                .filter(|category| !category.is_empty())
                .map(str::to_owned)
                .collect(),
        }),
    };
    request
        .validate()
        .map_err(|reason| ads_input_error(site, reason))?;
    Ok(request)
}

fn build_paused_adset_request(
    site: &str,
    options: PausedAdsetOptions,
) -> Result<CreatePausedAdRequest, Error> {
    let bid_strategy = BidStrategy::from_str(&options.bid_strategy)
        .map_err(|reason| ads_input_error(site, reason))?;
    let targeting = serde_json::from_str(&options.targeting)
        .map_err(|_| ads_input_error(site, "bad_targeting_json"))?;
    let request = CreatePausedAdRequest {
        account: options.ad_account,
        create: PausedAdCreate::Adset(PausedAdset {
            name: options.name,
            campaign_id: options.campaign_id,
            daily_budget: options.daily_budget,
            bid_strategy,
            billing_event: options.billing_event,
            optimization_goal: options.optimization_goal,
            targeting,
        }),
    };
    request
        .validate()
        .map_err(|reason| ads_input_error(site, reason))?;
    Ok(request)
}

/// The many required ad-set fields travel together from Clap to the pure
/// builder. This keeps adding one targeting option from turning the builder
/// into an error-prone positional parameter list.
struct PausedAdsetOptions {
    ad_account: Option<String>,
    name: String,
    campaign_id: String,
    daily_budget: u64,
    bid_strategy: String,
    billing_event: String,
    optimization_goal: String,
    targeting: String,
}

fn build_paused_ad_request(
    site: &str,
    ad_account: Option<String>,
    name: &str,
    adset_id: &str,
    creative_id: &str,
) -> Result<CreatePausedAdRequest, Error> {
    let request = CreatePausedAdRequest {
        account: ad_account,
        create: PausedAdCreate::Ad(PausedAd {
            name: name.into(),
            adset_id: adset_id.into(),
            creative_id: creative_id.into(),
        }),
    };
    request
        .validate()
        .map_err(|reason| ads_input_error(site, reason))?;
    Ok(request)
}

/// CLI flags → `InsightsQuery`. Pure over its inputs so the parse errors
/// (`unknown_*`, date and range problems) are unit-testable without HTTP.
fn build_insights_query(
    site: &str,
    from: &str,
    to: &str,
    level: &str,
    metrics: &str,
    attribution: &str,
    options: InsightsOptions,
) -> Result<InsightsQuery, Error> {
    let bad = |reason: String| Error::InvalidQuery {
        site: Site::new(site),
        reason,
    };
    let level = InsightsLevel::from_str(level).map_err(bad)?;
    let attribution = AttributionWindow::from_str(attribution).map_err(bad)?;
    let mut parsed = Vec::new();
    for m in metrics.split(',') {
        let m = m.trim();
        if m.is_empty() {
            continue;
        }
        parsed.push(Metric::from_str(m).map_err(bad)?);
    }
    if parsed.is_empty() {
        return Err(bad("no_metrics".into()));
    }
    let mut parsed_breakdowns = Vec::new();
    for breakdown in options.breakdowns.split(',') {
        let breakdown = breakdown.trim();
        if breakdown.is_empty() {
            continue;
        }
        parsed_breakdowns.push(Breakdown::from_str(breakdown).map_err(bad)?);
    }
    let range = DateRange {
        from: from.into(),
        to: to.into(),
    };
    Ok(InsightsQuery {
        level,
        metrics: parsed,
        attribution,
        range,
        account: options.ad_account,
        entity_ids: options.entity_ids,
        breakdowns: parsed_breakdowns,
    })
}

/// One human-mode row: `date level entity dimension=v metric=v …`. Keeping
/// dimensions before metrics prevents a country/platform label from looking
/// like a number that callers may sum across rows.
fn insight_line(row: &InsightRow) -> String {
    let mut parts = vec![
        row.date_start.clone(),
        row.level.as_str().to_string(),
        row.entity_id.clone(),
    ];
    for (k, v) in &row.dimensions {
        parts.push(format!("{k}={v}"));
    }
    for (k, v) in &row.metrics {
        parts.push(format!("{k}={v}"));
    }
    parts.join(" ")
}

/// Human output keeps the canonical `act_<id>` first so it can be copied
/// directly into `insights --ad-account`; names and metadata remain labels.
fn ad_account_line(account: &AdAccount) -> String {
    format!(
        "{} name={} currency={} timezone={} status={}",
        account.id,
        account.name.as_deref().unwrap_or("-"),
        account.currency.as_deref().unwrap_or("-"),
        account.timezone.as_deref().unwrap_or("-"),
        account.status.as_deref().unwrap_or("-")
    )
}

/// The id is adjacent to its entity name, followed by explicit `PAUSED`, so
/// it can be copied into Ads Manager without a human mistaking it for active.
fn created_ad_line(created: &CreatedAd) -> String {
    format!(
        "{} {} status={} account={}",
        created.entity.as_str(),
        created.id,
        created.status,
        created.account_id
    )
}

/// Vault home: `--home`/`POSTKIT_HOME` (clap folds the env var into the
/// flag) > `$HOME/.postkit`. Never falls back to the current directory:
/// with HOME unset (cron, systemd units, `env -i` shells) a CWD fallback
/// would silently write tokens into whatever directory the process
/// started in — possibly a checkout or a world-writable /tmp. Pure over
/// its inputs so the precedence is unit-testable without touching the
/// process environment.
fn resolve_home(
    flag_or_env: Option<PathBuf>,
    user_home: Option<PathBuf>,
) -> Result<PathBuf, String> {
    if let Some(p) = flag_or_env {
        return Ok(p);
    }
    user_home.map(|h| h.join(".postkit")).ok_or_else(|| {
        "POSTKIT_HOME or HOME must be set to locate the vault; refusing to guess from the current directory".into()
    })
}

fn check_name(s: &str, json: bool) -> Result<(), i32> {
    if valid_name(s) {
        Ok(())
    } else {
        Err(fail(&Error::InvalidName(s.into()), json))
    }
}

fn fail(e: &Error, json: bool) -> i32 {
    emit_err(e, json);
    e.exit_code()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn build_insights_query_parses_and_defaults() {
        let q = build_insights_query(
            "meta_ads",
            "2026-06-01",
            "2026-06-30",
            "campaign",
            "spend, purchases",
            "7d_click_1d_view",
            InsightsOptions {
                ad_account: Some("act_9".into()),
                entity_ids: vec!["238".into(), "239".into()],
                breakdowns: "country,age".into(),
            },
        )
        .unwrap();
        assert_eq!(q.level.as_str(), "campaign");
        assert_eq!(q.metrics, vec![Metric::Spend, Metric::Purchases]);
        assert_eq!(q.account.as_deref(), Some("act_9"));
        assert_eq!(q.entity_ids, vec!["238", "239"]);
        assert_eq!(q.breakdowns, vec![Breakdown::Country, Breakdown::Age]);
    }

    #[test]
    fn build_insights_query_rejects_bad_inputs() {
        // range errors surface via Client's validate(); parse errors here
        let cases: [(&str, &str, &str, &str); 4] = [
            (
                "campaigns",
                "spend",
                "7d_click_1d_view",
                "unknown_level:campaigns",
            ),
            (
                "campaign",
                "not_a_metric",
                "7d_click_1d_view",
                "unknown_metric:not_a_metric",
            ),
            (
                "campaign",
                "spend",
                "default",
                "unknown_attribution:default",
            ),
            ("campaign", " , ", "7d_click_1d_view", "no_metrics"),
        ];
        for (level, metrics, attribution, reason) in cases {
            let err = build_insights_query(
                "meta_ads",
                "2026-06-01",
                "2026-06-02",
                level,
                metrics,
                attribution,
                InsightsOptions {
                    ad_account: None,
                    entity_ids: vec![],
                    breakdowns: String::new(),
                },
            )
            .unwrap_err();
            assert!(
                matches!(&err, Error::InvalidQuery { reason: r, .. } if r == reason),
                "{level}/{metrics}/{attribution}: {err:?}"
            );
        }

        let err = build_insights_query(
            "meta_ads",
            "2026-06-01",
            "2026-06-02",
            "campaign",
            "roas",
            "7d_click_1d_view",
            InsightsOptions {
                ad_account: None,
                entity_ids: vec![],
                breakdowns: "country,unknown".into(),
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::InvalidQuery { reason, .. } if reason == "unknown_breakdown:unknown")
        );
    }

    #[test]
    fn insight_line_renders_row() {
        let mut metrics = serde_json::Map::new();
        metrics.insert("spend".into(), serde_json::json!(12.5));
        metrics.insert("impressions".into(), serde_json::json!(4567));
        let line = insight_line(&InsightRow {
            entity_id: "238".into(),
            level: InsightsLevel::Campaign,
            date_start: "2026-06-01".into(),
            dimensions: serde_json::Map::new(),
            metrics,
        });
        assert_eq!(line, "2026-06-01 campaign 238 impressions=4567 spend=12.5");
    }

    #[test]
    fn ad_account_line_is_copyable_and_labels_metadata() {
        let line = ad_account_line(&AdAccount {
            id: "act_123".into(),
            name: Some("Main".into()),
            currency: Some("ILS".into()),
            timezone: Some("Asia/Jerusalem".into()),
            status: Some("1".into()),
        });
        assert_eq!(
            line,
            "act_123 name=Main currency=ILS timezone=Asia/Jerusalem status=1"
        );
    }

    #[test]
    fn ads_accounts_command_parses() {
        let cli = Cli::try_parse_from(["postkit", "ads", "accounts", "meta_ads"]).unwrap();
        assert!(
            matches!(cli.command, Commands::Ads(AdsCmd::Accounts { site }) if site == "meta_ads")
        );
    }

    #[test]
    fn paused_ads_commands_and_builders_require_explicit_safe_inputs() {
        let cli = Cli::try_parse_from([
            "postkit",
            "ads",
            "create-campaign",
            "meta_ads",
            "--name",
            "Paused validation",
            "--objective",
            "sales",
            "--ad-account",
            "act_123",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Commands::Ads(AdsCmd::CreateCampaign { site, ad_account: Some(account), .. })
                if site == "meta_ads" && account == "act_123"
        ));

        // A missing strategy is a parser error rather than a Meta code 100:
        // strategies such as cost cap need additional constraint fields that
        // Tier B deliberately does not infer.
        let missing_bid_strategy = Cli::try_parse_from([
            "postkit",
            "ads",
            "create-adset",
            "meta_ads",
            "--name",
            "Paused ad set",
            "--campaign-id",
            "100",
            "--daily-budget",
            "2500",
            "--billing-event",
            "IMPRESSIONS",
            "--optimization-goal",
            "REACH",
            "--targeting-file",
            "targeting.json",
        ]);
        assert!(missing_bid_strategy.is_err());

        let adset_cli = Cli::try_parse_from([
            "postkit",
            "ads",
            "create-adset",
            "meta_ads",
            "--name",
            "Paused ad set",
            "--campaign-id",
            "100",
            "--daily-budget",
            "2500",
            "--bid-strategy",
            "lowest_cost_without_cap",
            "--billing-event",
            "IMPRESSIONS",
            "--optimization-goal",
            "REACH",
            "--targeting-file",
            "targeting.json",
        ])
        .unwrap();
        assert!(matches!(
            adset_cli.command,
            Commands::Ads(AdsCmd::CreateAdset { bid_strategy, .. })
                if bid_strategy == "lowest_cost_without_cap"
        ));

        let campaign = build_paused_campaign_request(
            "meta_ads",
            Some("act_123".into()),
            "Paused validation",
            "sales",
            "HOUSING, EMPLOYMENT",
        )
        .unwrap();
        assert!(matches!(
            campaign.create,
            PausedAdCreate::Campaign(PausedCampaign { objective: CampaignObjective::Sales, special_ad_categories, .. })
                if special_ad_categories == ["HOUSING", "EMPLOYMENT"]
        ));

        let adset = build_paused_adset_request(
            "meta_ads",
            PausedAdsetOptions {
                ad_account: None,
                name: "Paused ad set".into(),
                campaign_id: "100".into(),
                daily_budget: 2500,
                bid_strategy: "lowest_cost_without_cap".into(),
                billing_event: "IMPRESSIONS".into(),
                optimization_goal: "REACH".into(),
                targeting: r#"{"geo_locations":{"countries":["MY"]}}"#.into(),
            },
        )
        .unwrap();
        assert!(matches!(adset.create, PausedAdCreate::Adset(_)));

        let bad_bid_strategy = build_paused_adset_request(
            "meta_ads",
            PausedAdsetOptions {
                ad_account: None,
                name: "x".into(),
                campaign_id: "100".into(),
                daily_budget: 1,
                bid_strategy: "cost_cap".into(),
                billing_event: "IMPRESSIONS".into(),
                optimization_goal: "REACH".into(),
                targeting: "{}".into(),
            },
        );
        assert!(
            matches!(bad_bid_strategy, Err(Error::InvalidQuery { reason, .. }) if reason == "unknown_bid_strategy:cost_cap")
        );

        for (objective, targeting, reason) in [
            ("clicks", "{}", "unknown_objective:clicks"),
            ("sales", "[]", "targeting_must_be_object"),
            ("sales", "not json", "bad_targeting_json"),
        ] {
            let result = if objective == "clicks" {
                build_paused_campaign_request("meta_ads", None, "x", objective, "")
            } else {
                build_paused_adset_request(
                    "meta_ads",
                    PausedAdsetOptions {
                        ad_account: None,
                        name: "x".into(),
                        campaign_id: "100".into(),
                        daily_budget: 1,
                        bid_strategy: "lowest_cost_without_cap".into(),
                        billing_event: "IMPRESSIONS".into(),
                        optimization_goal: "REACH".into(),
                        targeting: targeting.into(),
                    },
                )
            };
            assert!(
                matches!(result, Err(Error::InvalidQuery { reason: actual, .. }) if actual == reason),
                "{objective}/{targeting} should be {reason}"
            );
        }
    }

    #[test]
    fn paused_create_line_cannot_hide_its_status() {
        let line = created_ad_line(&CreatedAd {
            site: Site::new("meta_ads"),
            account_id: "act_123".into(),
            entity: postkit::AdEntity::Campaign,
            id: "100".into(),
            status: "PAUSED".into(),
        });
        assert_eq!(line, "campaign 100 status=PAUSED account=act_123");
    }

    #[test]
    fn insights_help_lists_all_supported_metrics() {
        // Keep the user-facing discovery text aligned with Metric::from_str.
        // This caught the Tier A+ metrics being accepted by the parser but
        // absent from `postkit insights --help`.
        let mut command = Cli::command();
        let help = command
            .find_subcommand_mut("insights")
            .expect("insights command")
            .get_arguments()
            .find(|arg| arg.get_id() == "metrics")
            .and_then(|arg| arg.get_help())
            .expect("metrics help")
            .to_string();
        assert!(help.contains("purchase_value,roas"));
    }

    #[test]
    fn home_precedence_and_no_cwd_fallback() {
        let fe = Some(PathBuf::from("/flag-or-env"));
        let hm = Some(PathBuf::from("/user"));
        // --home / POSTKIT_HOME (same field via clap env) > $HOME/.postkit
        assert_eq!(
            resolve_home(fe.clone(), hm.clone()).unwrap(),
            PathBuf::from("/flag-or-env")
        );
        assert_eq!(
            resolve_home(None, hm.clone()).unwrap(),
            PathBuf::from("/user/.postkit")
        );
        // HOME unset (cron, systemd, env -i): never guess the CWD — the old
        // code silently wrote tokens into ./.postkit
        let err = resolve_home(None, None).unwrap_err();
        assert!(err.contains("POSTKIT_HOME or HOME"));
    }

    #[test]
    fn postkit_home_env_feeds_the_home_flag() {
        // clap's env feature folds POSTKIT_HOME into --home, so the var is
        // visible in --help and the manual env read stays out of run().
        std::env::set_var("POSTKIT_HOME", "/from-env");
        let cli = Cli::try_parse_from(["postkit", "whoami", "threads"]).unwrap();
        assert_eq!(cli.home, Some(PathBuf::from("/from-env")));
        std::env::remove_var("POSTKIT_HOME");
    }

    #[test]
    fn resolve_texts_requires_one() {
        assert_eq!(resolve_texts(vec![]).unwrap_err(), 2);
        assert_eq!(resolve_texts(vec!["a".into(), "-".into()]).unwrap_err(), 2);
        assert_eq!(
            resolve_texts(vec!["root".into(), "reply".into()]).unwrap(),
            vec!["root", "reply"]
        );
    }

    #[test]
    fn collect_post_sites_from_to_or_site() {
        assert_eq!(
            collect_post_sites(None, Some("threads, bluesky")).unwrap(),
            vec!["threads", "bluesky"]
        );
        assert_eq!(
            collect_post_sites(Some("threads"), None).unwrap(),
            vec!["threads"]
        );
        assert_eq!(collect_post_sites(None, None).unwrap_err(), 2);
    }

    #[test]
    fn chain_blocked_unless_all_threads() {
        assert_eq!(chain_blocked_site(&["threads".into()]), None);
        assert_eq!(
            chain_blocked_site(&["threads".into(), "threads".into()]),
            None
        );
        assert_eq!(
            chain_blocked_site(&["threads".into(), "bluesky".into()]),
            Some("bluesky")
        );
    }

    #[test]
    fn with_reply_to_overwrites_parent() {
        let p = with_reply_to(&serde_json::json!({ "reply_to_id": "old" }), "A");
        assert_eq!(p["reply_to_id"], "A");
        let p = with_reply_to(&serde_json::json!({}), "B");
        assert_eq!(p["reply_to_id"], "B");
    }

    #[test]
    fn result_line_matches_outcome_and_wire_error() {
        let ok = serde_json::json!({ "site": "threads", "id": "9", "url": "https://x/1" });
        assert_eq!(result_line(&ok), "threads 9 https://x/1");
        let no_url = serde_json::json!({ "site": "bluesky", "id": "at://x" });
        assert_eq!(result_line(&no_url), "bluesky at://x");
        let err = serde_json::json!({ "error": "invalid_post", "site": "threads", "reason": "text_too_long" });
        assert_eq!(result_line(&err), "invalid_post threads text_too_long");
        let terse = serde_json::json!({ "error": "rate_limited", "site": "threads" });
        assert_eq!(result_line(&terse), "rate_limited threads");
        // 027: probe rows must never render like posts — the id is an
        // unpublished container, and a bare "threads C" would read as one
        let probe =
            serde_json::json!({ "site": "threads", "container_id": "C", "expires_in_hours": 24 });
        assert_eq!(result_line(&probe), "threads C dry-run");
    }

    #[test]
    fn dry_run_flag_parses() {
        let cli = Cli::try_parse_from(["postkit", "post", "threads", "--text", "hi", "--dry-run"])
            .unwrap();
        match cli.command {
            Commands::Post { dry_run, .. } => assert!(dry_run),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn dry_run_conflicts_are_refused_loudly() {
        // no dry-run: no opinion, whatever the other flags say
        assert!(dry_run_conflict(false, Some("k"), 3).is_none());
        // dry-run + idempotency: a probe never touches the ledger
        let e = dry_run_conflict(true, Some("k"), 1).unwrap();
        assert!(matches!(&e, Error::InvalidPost { reason, .. } if reason == "dry_run_idempotency"));
        // dry-run + chain: no published parent to reply to
        let e = dry_run_conflict(true, None, 2).unwrap();
        assert!(matches!(&e, Error::InvalidPost { reason, .. } if reason == "dry_run_chain"));
        // dry-run alone with one text is exactly the intended shape
        assert!(dry_run_conflict(true, None, 1).is_none());
    }
}
