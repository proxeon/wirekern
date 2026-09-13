//! X-specific CLI commands kept outside the generic public-post grammar.

use crate::app::fail;
use crate::output::emit_ok;
use clap::Subcommand;
use postkit::{AccountKey, Client, Deadline, XDirectMessageRequest};

#[derive(Subcommand, Debug)]
pub(crate) enum XCmd {
    /// Send one private, one-to-one text direct message. This needs OAuth
    /// credentials authorized with `postkit auth x --with-dm` and an explicit
    /// per-invocation acknowledgement because it is not a public post.
    Dm {
        /// Numeric X user ID, not @handle.
        #[arg(long = "to")]
        recipient_id: String,
        #[arg(long)]
        text: String,
        /// Required local idempotency key; retry the exact send with it.
        #[arg(long)]
        idempotency: String,
        /// Acknowledge this private outbound message intentionally.
        #[arg(long)]
        allow_dm: bool,
    },
}

pub(crate) fn direct_message_allowed(command: &XCmd) -> bool {
    matches!(command, XCmd::Dm { allow_dm: true, .. })
}

pub(crate) async fn dispatch(
    client: Client,
    command: XCmd,
    json: bool,
    account: String,
    deadline: Deadline,
) -> Result<(), i32> {
    match command {
        XCmd::Dm {
            recipient_id,
            text,
            idempotency,
            allow_dm,
        } => {
            if !allow_dm {
                eprintln!("x dm requires --allow-dm");
                return Err(2);
            }
            let request = XDirectMessageRequest {
                recipient_id,
                text,
                idempotency_key: idempotency,
            };
            match client
                .send_x_direct_message(&AccountKey::new("x", account), request, deadline)
                .await
            {
                Ok(outcome) => {
                    emit_ok(&outcome, json, || {
                        format!(
                            "x DM {} accepted by X (delivery is reported separately)",
                            outcome.id.as_deref().unwrap_or("-")
                        )
                    });
                    Ok(())
                }
                Err(error) => Err(fail(&error, json)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dm_acknowledgement_is_not_implicit() {
        let denied = XCmd::Dm {
            recipient_id: "1".into(),
            text: "hi".into(),
            idempotency: "dm-1".into(),
            allow_dm: false,
        };
        assert!(!direct_message_allowed(&denied));
    }
}
