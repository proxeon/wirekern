mod output;

use clap::{Parser, Subcommand};
use output::{emit_err, emit_ok, emit_raw};
use postkit::{
    extract_code, query_param, valid_name, AccountKey, AppConfig, AppStore, AuthReply, Body,
    Client, Deadline, Error, FileAppStore, FileVault, Intent, OAuthApp, PostRequest, Registry,
    Site, Vault,
};
use postkit::connectors::threads::validate_text;
use std::io::{self, BufRead, IsTerminal, Read};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(name = "postkit", version, about = "Publish to official APIs. BYO credentials.")]
struct Cli {
    /// JSON document on stdout (agents). Human text on stderr otherwise.
    #[arg(long, global = true)]
    json: bool,
    /// Vault root. Default ~/.postkit
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    /// Seconds for publish. Default 30.
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
    let home = cli.home.unwrap_or_else(default_home);
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
            let id = cfg.oauth.as_ref().map(|o| o.client_id.as_str()).unwrap_or("");
            let redir = cfg
                .oauth
                .as_ref()
                .map(|o| o.redirect_uri.as_str())
                .unwrap_or("");
            if json {
                emit_raw(&serde_json::json!({
                    "site": site,
                    "client_id": id,
                    "redirect_uri": redir,
                }));
            } else {
                println!("site={site} client_id={id} redirect_uri={redir} client_secret=[redacted]");
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
                    println!("{}/{}", k.site, k.name);
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
            dispatch(client, other, json, account, deadline).await
        }
    }
}

async fn dispatch(
    client: Client,
    cmd: Commands,
    json: bool,
    account: String,
    deadline: Deadline,
) -> Result<(), i32> {
    match cmd {
        Commands::Capabilities { site } => {
            if let Some(s) = site {
                let site = Site::new(s);
                match client.registry().capabilities_for(&site) {
                    Some(caps) => {
                        let names: Vec<&str> = caps.iter().map(|c| c.as_str()).collect();
                        if json {
                            emit_raw(&serde_json::json!({ site.as_str(): names }));
                        } else {
                            println!("{}: {}", site, names.join(", "));
                        }
                    }
                    None => {
                        return Err(fail(&Error::UnknownSite(site), json));
                    }
                }
            } else if json {
                emit_raw(&client.registry().capabilities_json());
            } else {
                println!("{}", client.registry().capabilities_json());
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
                    eprint!("app password: ");
                    let mut line = String::new();
                    io::stdin().lock().read_line(&mut line).map_err(|_| 5)?;
                    line.trim().to_string()
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
                client
                    .auth_finish(&key, AuthReply::Pasted { code })
                    .await
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
                            if let Some(got) = query_param(line.trim(), "state") {
                                if got != state {
                                    return Err(fail(
                                        &Error::Auth {
                                            site: Site::new(&site),
                                            reason: "state_mismatch".into(),
                                        },
                                        json,
                                    ));
                                }
                            }
                            let code = extract_code(&line).map_err(|e| fail(&e, json))?;
                            client
                                .auth_finish(&key, AuthReply::Pasted { code })
                                .await
                        }
                        postkit::AuthStart::PasteInstructions { hint } => {
                            eprintln!("{hint}");
                            if !io::stdin().is_terminal() {
                                eprintln!("then: postkit auth {site} --account <handle> --password <app-password>");
                                return Ok(());
                            }
                            eprint!("app password: ");
                            let mut line = String::new();
                            io::stdin().lock().read_line(&mut line).map_err(|_| 5)?;
                            client
                                .auth_finish(
                                    &key,
                                    AuthReply::AppPassword {
                                        identifier: account.clone(),
                                        secret: line.trim().to_string(),
                                        pds: None,
                                    },
                                )
                                .await
                        }
                        postkit::AuthStart::None => {
                            eprintln!(
                                "this site does not use OAuth; pass --token or --password"
                            );
                            return Ok(());
                        }
                    },
                    Err(e) => Err(e),
                }
            };
            match result {
                Ok(w) => {
                    emit_ok(&w, json, || {
                        format!("ok {} {}", w.site, w.handle.as_deref().unwrap_or(&w.id))
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
        } => {
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
                    match client.publish(&key, intent, deadline).await {
                        Ok(o) => results.push(serde_json::to_value(&o).unwrap()),
                        Err(e) => {
                            if code == 0 {
                                code = e.exit_code();
                            }
                            results.push(serde_json::to_value(postkit::WireError::from(&e)).unwrap());
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
                one_post(&client, &key, intent, deadline, json).await
            }
        }
        _ => unreachable!(),
    }
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

/// `--json` prints `{ "results": [...] }`; human mode one line per result.
fn print_results(results: &[serde_json::Value], json: bool) {
    if json {
        emit_raw(&serde_json::json!({ "results": results }));
    } else {
        for r in results {
            println!("{}", result_line(r));
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
    registry.register(Arc::new(
        postkit::connectors::threads::Threads::new()?,
    ));
    registry.register(Arc::new(
        postkit::connectors::bluesky::Bluesky::new()?,
    ));
    let vault = Arc::new(FileVault::new(home)?);
    let apps = Arc::new(FileAppStore::new(home)?);
    Ok(Client::new(registry, vault, apps))
}

fn default_home() -> PathBuf {
    std::env::var_os("POSTKIT_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs_home()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".postkit")
        })
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
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

    #[test]
    fn resolve_texts_requires_one() {
        assert_eq!(resolve_texts(vec![]).unwrap_err(), 2);
        assert_eq!(
            resolve_texts(vec!["a".into(), "-".into()]).unwrap_err(),
            2
        );
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
    }
}
