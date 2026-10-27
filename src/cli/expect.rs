//! `expect …`: waiting for a condition on the server; a timeout is a failed check.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use mumbot::{Bot, MessageCheck, UserCheck};
use mumcord_protocol::{DenyKind, permissions};

#[derive(Subcommand, Debug)]
pub enum Expect {
    /// Another user's state.
    User {
        name: String,
        #[command(flatten)]
        check: UserCheck,
        /// Avatar must equal this file's bytes.
        #[arg(long)]
        avatar_file: Option<PathBuf>,
    },
    /// No user with this name is connected.
    UserGone { name: String },
    /// A received text message (since this run connected).
    Message {
        #[command(flatten)]
        check: MessageCheck,
    },
    /// A request of this bot was denied (since this run connected).
    Denied {
        /// Missing permission, e.g. listen, enter, speak, text, self-register.
        #[arg(long)]
        permission: Option<String>,
    },
}

pub async fn run(bot: &Bot, expect: Expect) -> Result<()> {
    match expect {
        Expect::User {
            name,
            mut check,
            avatar_file,
        } => {
            if let Some(path) = avatar_file {
                check.avatar = Some(
                    std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?,
                );
            }
            bot.wait_user(&name, &check).await?;
        }
        Expect::UserGone { name } => bot.wait_user_gone(&name).await?,
        Expect::Message { check } => {
            let msg = bot.wait_message(&check).await?;
            println!("{}", msg.message);
        }
        Expect::Denied { permission } => {
            let wanted = permission.as_deref().map(permission_bit).transpose()?;
            let what = permission.unwrap_or_else(|| "any".to_owned());
            let denial = bot
                .wait_denied(&what, |d| {
                    wanted.is_none_or(|bit| {
                        d.kind == DenyKind::Permission && d.permission == Some(bit)
                    })
                })
                .await?;
            println!("{:?}", denial.kind);
        }
    }
    Ok(())
}

fn permission_bit(name: &str) -> Result<u32> {
    Ok(match name {
        "write" => permissions::WRITE,
        "traverse" => permissions::TRAVERSE,
        "enter" => permissions::ENTER,
        "speak" => permissions::SPEAK,
        "whisper" => permissions::WHISPER,
        "text" => permissions::TEXT_MESSAGE,
        "listen" => permissions::LISTEN,
        "register" => permissions::REGISTER,
        "self-register" => permissions::SELF_REGISTER,
        other => bail!("unknown permission {other:?}"),
    })
}
