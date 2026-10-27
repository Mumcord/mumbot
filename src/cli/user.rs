//! The bot's own identity and state: connect, channel, name, avatar, comment, registration,
//! listening, self mute and deafen.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use mumbot::Bot;
use mumbot::samples::PNG;

use super::Toggle;

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Connect and print session and certificate hash.
    Connect,
    /// Move into a channel (name or Root/Path).
    Join { channel: String },
    /// Reconnect under a new name, keeping the certificate.
    Rename { new_name: String },
    /// Set the avatar from an image file (or the built-in sample with --sample).
    Avatar {
        file: Option<PathBuf>,
        #[arg(long, conflicts_with = "file")]
        sample: bool,
        /// Remove the avatar.
        #[arg(long, conflicts_with_all = ["file", "sample"])]
        clear: bool,
    },
    /// Set the comment (HTML; "" removes it).
    Comment { html: String },
    /// Self-register on the server under the current name.
    Register,
    /// Listen to (follow) channels.
    Listen { channels: Vec<String> },
    /// Stop listening to channels.
    Unlisten { channels: Vec<String> },
    /// Self mute on/off.
    Mute { on: Toggle },
    /// Self deafen on/off.
    Deaf { on: Toggle },
}

pub async fn run(bot: &mut Bot, command: Command) -> Result<()> {
    match command {
        Command::Connect => print_identity(bot)?,
        Command::Join { channel } => bot.join(&channel).await?,
        Command::Rename { new_name } => {
            bot.reconnect_as(&new_name).await?;
            print_identity(bot)?;
        }
        Command::Avatar {
            file,
            sample,
            clear,
        } => {
            let image = match (file, sample, clear) {
                (Some(path), _, _) => {
                    std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?
                }
                (None, true, _) => PNG.to_vec(),
                (None, false, true) => Vec::new(),
                (None, false, false) => bail!("give an image file, --sample or --clear"),
            };
            bot.set_avatar(image)?;
        }
        Command::Comment { html } => bot.set_comment(&html)?,
        Command::Register => bot.register().await?,
        Command::Listen { channels } => bot.listen(&as_strs(&channels))?,
        Command::Unlisten { channels } => bot.unlisten(&as_strs(&channels))?,
        Command::Mute { on } => bot.set_mute(on.on())?,
        Command::Deaf { on } => bot.set_deaf(on.on())?,
    }
    Ok(())
}

fn print_identity(bot: &Bot) -> Result<()> {
    let user = bot.own_user()?;
    println!(
        "session={} name={} cert_hash={}",
        user.session,
        user.name,
        user.cert_hash.as_deref().unwrap_or("-")
    );
    Ok(())
}

fn as_strs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}
