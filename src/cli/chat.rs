//! Text chat: channel and private messages, canned samples for rendering tests.

use anyhow::Result;
use clap::Subcommand;
use mumbot::Bot;
use mumbot::samples::Sample;

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Send HTML to a channel (default: the current one).
    Say {
        #[arg(long)]
        channel: Option<String>,
        html: String,
    },
    /// Send a canned message (formatting/sanitizer tests).
    SaySample {
        #[arg(long)]
        channel: Option<String>,
        sample: Sample,
    },
    /// Send HTML privately to a user.
    Pm { user: String, html: String },
}

pub fn run(bot: &Bot, command: Command) -> Result<()> {
    match command {
        Command::Say { channel, html } => bot.send_channel(channel.as_deref(), &html),
        Command::SaySample { channel, sample } => {
            bot.send_channel(channel.as_deref(), &sample.html())
        }
        Command::Pm { user, html } => bot.send_private(&user, &html),
    }
}
