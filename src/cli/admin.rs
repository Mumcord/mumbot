//! Superuser actions: moderating other users and changing channel ACLs and descriptions.

use anyhow::Result;
use clap::Subcommand;
use mumbot::{Bot, Moderation};

use super::Toggle;

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Server-mute or -deafen another user (needs --superuser).
    Moderate {
        user: String,
        #[arg(long)]
        mute: Option<Toggle>,
        #[arg(long)]
        deaf: Option<Toggle>,
    },
    /// Move another user into a channel (needs --superuser).
    Move { user: String, channel: String },
    /// Deny "Speak" for everyone on a channel, which suppresses them (needs --superuser).
    DenySpeak { channel: String },
    /// Undo deny-speak (needs --superuser).
    AllowSpeak { channel: String },
    /// Deny "Listen" for everyone on a channel (needs --superuser).
    DenyListen { channel: String },
    /// Undo deny-listen (needs --superuser).
    AllowListen { channel: String },
    /// Set a channel's description; an empty HTML removes it (needs --superuser).
    Describe { channel: String, html: String },
}

pub async fn run(bot: &Bot, command: Command) -> Result<()> {
    match command {
        Command::Moderate { user, mute, deaf } => bot.moderate(
            &user,
            Moderation {
                mute: mute.map(Toggle::on),
                deaf: deaf.map(Toggle::on),
            },
        ),
        Command::Move { user, channel } => bot.move_user(&user, &channel),
        Command::DenySpeak { channel } => bot.deny_speak(&channel).await,
        Command::AllowSpeak { channel } => bot.allow_speak(&channel).await,
        Command::DenyListen { channel } => bot.deny_listen(&channel).await,
        Command::AllowListen { channel } => bot.allow_listen(&channel).await,
        Command::Describe { channel, html } => bot.set_description(&channel, &html).await,
    }
}
