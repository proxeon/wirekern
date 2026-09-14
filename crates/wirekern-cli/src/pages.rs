//! `wirekern pages` — Facebook Page identity discovery.
use crate::app::fail;
use crate::output::{emit_raw, human_line};
use clap::Subcommand;
use wirekern::{AccountKey, Client, Deadline};

#[derive(Subcommand, Debug)]
pub(crate) enum PagesCmd {
    /// List Pages visible to the selected Facebook Pages credential.
    Accounts { site: String },
}

pub(crate) async fn dispatch(
    client: &Client,
    cmd: PagesCmd,
    json: bool,
    account: &str,
    deadline: Deadline,
) -> Result<(), i32> {
    match cmd {
        PagesCmd::Accounts { site } => {
            let key = AccountKey::new(&site, account);
            match client.pages(&key, deadline).await {
                Ok(reply) => {
                    if json {
                        emit_raw(&serde_json::to_value(&reply).expect("json"));
                    } else {
                        for page in &reply.pages {
                            let name = page.name.as_deref().unwrap_or("-");
                            let tasks = if page.tasks.is_empty() {
                                "-".to_string()
                            } else {
                                page.tasks.join(",")
                            };
                            human_line(format!("{} {} tasks={tasks}", page.id, name));
                        }
                    }
                    Ok(())
                }
                Err(error) => Err(fail(&error, json)),
            }
        }
    }
}
