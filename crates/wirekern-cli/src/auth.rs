//! `wirekern auth` and `wirekern whoami`.
use crate::app::fail;
use crate::output::emit_ok;
use std::io::{self, BufRead, IsTerminal};
use std::path::Path;
use wirekern::{
    extract_code, verify_state, AccountKey, AuthReply, AuthStartOptions, Client, Deadline,
};

pub(crate) async fn whoami(
    client: &Client,
    site: String,
    json: bool,
    account: &str,
) -> Result<(), i32> {
    let key = AccountKey::new(&site, account);
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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run(
    client: &Client,
    home: &Path,
    site: String,
    token: Option<String>,
    code: Option<String>,
    password: Option<String>,
    system_user: bool,
    with_dm: bool,
    with_replies: bool,
    json: bool,
    account: String,
    deadline: Deadline,
) -> Result<(), i32> {
    let key = AccountKey::new(&site, &account);
    if with_dm && site != "x" {
        eprintln!("--with-dm is only valid for x");
        return Err(2);
    }
    if with_replies && site != "threads" {
        eprintln!("--with-replies is only valid for threads");
        return Err(2);
    }
    if with_dm && with_replies {
        eprintln!("--with-dm and --with-replies cannot be combined");
        return Err(2);
    }
    let result = if system_user {
        if site != "meta_ads" {
            eprintln!("--system-user is only valid for meta_ads");
            return Err(2);
        }
        let Some(token) = token else {
            eprintln!("--system-user requires --token");
            return Err(2);
        };
        if code.is_some() || password.is_some() {
            eprintln!("--system-user cannot be combined with --code or --password");
            return Err(2);
        }
        client
            .put_ads_system_user_token(&key, &token, deadline)
            .await
    } else if let Some(password) = password {
        if token.is_some() || code.is_some() {
            eprintln!("--password cannot be combined with --token or --code");
            return Err(2);
        }
        let secret = if password.is_empty() {
            if !io::stdin().is_terminal() {
                eprintln!(
                    "then: wirekern auth {site} --account <handle> --password <app-password>"
                );
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
        // PKCE needs the redirect state to retrieve its vault-held verifier.
        // Keep a pasted URL intact; older OAuth connectors still receive a
        // normalized code through the Pasted branch.
        let looks_url = code.contains("://") || code.starts_with("http");
        if looks_url {
            client
                .auth_finish(&key, AuthReply::Redirect { url: code })
                .await
        } else {
            let code = extract_code(&code).map_err(|e| fail(&e, json))?;
            client.auth_finish(&key, AuthReply::Pasted { code }).await
        }
    } else {
        let options = AuthStartOptions {
            requested_features: [
                with_dm.then(|| "direct_messages".into()),
                with_replies.then(|| "replies".into()),
            ]
            .into_iter()
            .flatten()
            .collect(),
        };
        match client.auth_start_for(&key, options).await {
            Ok(start) => match start {
                wirekern::AuthStart::Browser {
                    authorize_url,
                    state,
                    ..
                } => {
                    eprintln!("open: {authorize_url}");
                    if !io::stdin().is_terminal() {
                        eprintln!("then: wirekern auth {site} --code <code>");
                        return Ok(());
                    }
                    eprintln!("paste the redirected URL or code, then Enter");
                    let mut line = String::new();
                    io::stdin().lock().read_line(&mut line).map_err(|_| 5)?;
                    verify_state(&state, &line).map_err(|e| fail(&e, json))?;
                    // Preserve a redirect so Client can match its PKCE state;
                    // raw codes remain supported for legacy non-PKCE sites.
                    let trimmed = line.trim().to_string();
                    let reply = if trimmed.contains("://") || trimmed.starts_with("http") {
                        AuthReply::Redirect { url: trimmed }
                    } else {
                        AuthReply::Pasted {
                            code: extract_code(&trimmed).map_err(|e| fail(&e, json))?,
                        }
                    };
                    client.auth_finish(&key, reply).await
                }
                wirekern::AuthStart::PasteInstructions { hint } => {
                    eprintln!("{hint}");
                    if !io::stdin().is_terminal() {
                        eprintln!("then: wirekern auth {site} --account <handle> --password <app-password>");
                        return Ok(());
                    }
                    // Echo suppressed for the same reason as the
                    // --password prompt above.
                    let secret = rpassword::prompt_password("app password: ").map_err(|_| 5)?;
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
                wirekern::AuthStart::None => {
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
