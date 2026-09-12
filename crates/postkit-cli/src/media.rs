//! `postkit media` — bounded first-page published-media reads.
use crate::ads::media_line;
use crate::app::fail;
use crate::output::{emit_raw, human_line};
use clap::Subcommand;
use postkit::{AccountKey, Client, Deadline, MediaQuery, DEFAULT_MEDIA_LIMIT};

#[derive(Subcommand, Debug)]
pub(crate) enum MediaCmd {
    /// List recent published media for the credential's explicit account.
    List {
        site: String,
        /// First-page size, 1 through 25. Postkit intentionally exposes no
        /// pagination cursor until that larger read contract is reviewed.
        #[arg(long, default_value_t = DEFAULT_MEDIA_LIMIT)]
        limit: u8,
    },
}

pub(crate) async fn dispatch(
    client: &Client,
    cmd: MediaCmd,
    json: bool,
    account: &str,
    deadline: Deadline,
) -> Result<(), i32> {
    match cmd {
        MediaCmd::List { site, limit } => {
            let key = AccountKey::new(&site, account);
            match client.media(&key, MediaQuery { limit }, deadline).await {
                Ok(reply) => {
                    if json {
                        emit_raw(&serde_json::to_value(&reply).expect("json"));
                    } else {
                        for media in &reply.media {
                            human_line(media_line(media));
                        }
                    }
                    Ok(())
                }
                Err(error) => Err(fail(&error, json)),
            }
        }
    }
}
