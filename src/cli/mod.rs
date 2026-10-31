//! The scripted test bot's subcommands, one module per area. The area enums are flattened into
//! the top level of the `mumbot` CLI, so the command line stays the one Mumcord's tests use
//! (`mumbot say …`, not `mumbot test chat say …`).

mod admin;
mod chat;
mod expect;
mod user;
mod voice;

use std::io::BufRead;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use mumbot::Bot;
use mumbot::voice::BotVoice;
use mumbot::voice_settings::VoiceSettings;

#[derive(Subcommand, Debug)]
pub enum Command {
    #[command(flatten)]
    User(user::Command),
    #[command(flatten)]
    Chat(chat::Command),
    #[command(flatten)]
    Admin(admin::Command),
    /// Wait for a condition; exits non-zero on timeout.
    #[command(subcommand)]
    Expect(expect::Expect),
    #[command(flatten)]
    Voice(voice::Command),
    /// Wait a number of seconds (pacing in scripts).
    Sleep { seconds: f64 },
    /// Run subcommands from stdin, one per line (shell-like quoting, # comments), on one
    /// connection; prints "ok <line>" after each.
    Script,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum Toggle {
    On,
    Off,
}

impl Toggle {
    fn on(self) -> bool {
        matches!(self, Toggle::On)
    }
}

/// One connected bot and, once a voice command ran, its voice.
pub struct Session {
    bot: Bot,
    voice: Option<BotVoice>,
    /// What `--bitrate`, `--frame-ms` and the environment asked for.
    voice_settings: VoiceSettings,
}

impl Session {
    /// Voice is started on first use and kept for the rest of the run.
    async fn voice(&mut self) -> Result<&BotVoice> {
        if self.voice.is_none() {
            self.voice = Some(BotVoice::start(&self.bot).await?);
        }
        self.voice.as_ref().context("voice not started")
    }
}

/// Runs the command given on the command line (`script` reads further commands from stdin),
/// stays connected for `hold` after it succeeded and then disconnects cleanly, so everything the
/// bot sent reaches the server first. A failed command ends the run without the hold.
pub async fn run_top_level(
    bot: Bot,
    command: Command,
    hold: Duration,
    voice_settings: VoiceSettings,
) -> Result<()> {
    let mut session = Session {
        bot,
        voice: None,
        voice_settings,
    };
    let result = match command {
        Command::Script => run_script(&mut session).await,
        command => run(&mut session, command).await,
    };
    if result.is_ok() {
        tokio::time::sleep(hold).await;
    }
    let Session { bot, voice, .. } = session;
    drop(voice);
    let disconnected = bot.disconnect().await;
    result?;
    disconnected
}

async fn run_script(session: &mut Session) -> Result<()> {
    for (number, line) in std::io::stdin().lock().lines().enumerate() {
        let line = line.context("reading stdin")?;
        let words = split_words(&line).with_context(|| format!("line {}", number + 1))?;
        if words.is_empty() || words[0].starts_with('#') {
            continue;
        }
        let parsed = ScriptLine::try_parse_from(std::iter::once("script".to_owned()).chain(words))
            .with_context(|| format!("line {}: {line}", number + 1))?;
        if matches!(parsed.command, Command::Script) {
            bail!("line {}: script cannot be nested", number + 1);
        }
        run(session, parsed.command)
            .await
            .with_context(|| format!("line {}: {line}", number + 1))?;
        println!("ok {line}");
    }
    Ok(())
}

#[derive(Parser, Debug)]
struct ScriptLine {
    #[command(subcommand)]
    command: Command,
}

async fn run(session: &mut Session, command: Command) -> Result<()> {
    match command {
        Command::User(command) => user::run(&mut session.bot, command).await,
        Command::Chat(command) => chat::run(&session.bot, command),
        Command::Admin(command) => admin::run(&session.bot, command).await,
        Command::Expect(expect) => expect::run(&session.bot, expect).await,
        Command::Voice(command) => voice::run(session, command).await,
        Command::Sleep { seconds } => {
            tokio::time::sleep(secs(seconds)?).await;
            Ok(())
        }
        Command::Script => bail!("script is only valid as the top-level command"),
    }
}

fn secs(seconds: f64) -> Result<Duration> {
    Duration::try_from_secs_f64(seconds).context("bad duration")
}

/// Splits a script line like a shell: whitespace separates, '…' and "…" quote, \ escapes.
fn split_words(line: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut quote = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"') | None, '\\') => {
                let escaped = chars.next().context("dangling backslash")?;
                word.get_or_insert_default().push(escaped);
            }
            (Some(_), c) => word.get_or_insert_default().push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                word.get_or_insert_default();
            }
            (None, c) if c.is_whitespace() => words.extend(word.take()),
            (None, c) => word.get_or_insert_default().push(c),
        }
    }
    if quote.is_some() {
        bail!("unterminated quote");
    }
    words.extend(word);
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Result<Command, clap::Error> {
        ScriptLine::try_parse_from(
            std::iter::once("script".to_owned()).chain(split_words(line).expect("split")),
        )
        .map(|l| l.command)
    }

    #[test]
    fn splits_like_a_shell() {
        let words =
            split_words(r#"say --channel Raid "<b>hi</b> there" 'a "b"' x\ y """#).expect("split");
        assert_eq!(
            words,
            [
                "say",
                "--channel",
                "Raid",
                "<b>hi</b> there",
                "a \"b\"",
                "x y",
                ""
            ]
        );
        assert!(split_words("say \"open").is_err());
    }

    #[test]
    fn script_lines_parse_as_subcommands() {
        assert!(matches!(
            parse("expect user Alice --in Raid --muted true"),
            Ok(Command::Expect(expect::Expect::User { .. }))
        ));
        assert!(matches!(
            parse("say-sample rich"),
            Ok(Command::Chat(chat::Command::SaySample { .. }))
        ));
        assert!(matches!(
            parse("play a.wav --position -3,0,0"),
            Ok(Command::Voice(voice::Command::Play {
                position: Some([x, _, _]),
                ..
            })) if x == -3.0
        ));
        assert!(parse("nonsense").is_err());
    }
}
