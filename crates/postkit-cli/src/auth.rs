//! `postkit auth` and `postkit whoami`.
use crate::app::fail;
use crate::output::emit_ok;
use postkit::{extract_code, verify_state, AccountKey, AuthReply, Client, Deadline, Site};
use std::io::{self, BufRead, IsTerminal};
use std::path::Path;

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
    listen: bool,
    password: Option<String>,
    system_user: bool,
    json: bool,
    account: String,
    deadline: Deadline,
) -> Result<(), i32> {
    if listen {
        eprintln!("--listen is not implemented in this scaffold; paste the code instead");
        return Err(2);
    }
    let key = AccountKey::new(&site, &account);
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
