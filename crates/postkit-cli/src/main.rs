mod output;

use clap::{Parser, Subcommand};
use output::{emit_err, emit_ok, emit_raw};
use postkit::{
    extract_code, query_param, valid_name, AccountKey, AppConfig, AppStore, AuthReply, Body,
    Client, Deadline, Error, FileAppStore, FileVault, Intent, OAuthApp, PostRequest, Registry,
    Site, Vault,
};
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
        #[arg(long)]
        text: Option<String>,
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
        #[arg(long)]
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
            password: _,
        } => {
            if listen {
                eprintln!("--listen is not implemented in this scaffold; paste the code instead");
                return Err(2);
            }
            let key = AccountKey::new(&site, &account);
            let result = if let Some(token) = token {
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
                            return Ok(());
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
            let text = match text {
                Some(t) if t == "-" => {
                    let mut buf = String::new();
                    io::stdin().read_to_string(&mut buf).map_err(|_| 5)?;
                    buf
                }
                Some(t) => t,
                None => {
                    eprintln!("--text is required (or --stdin)");
                    return Err(2);
                }
            };
            let params = parse_params(&param, json)?;
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
                emit_raw(&serde_json::json!({ "results": results }));
                if code == 0 {
                    Ok(())
                } else {
                    Err(code)
                }
            } else {
                let site = site.ok_or_else(|| {
                    eprintln!("site or --to is required");
                    2
                })?;
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
