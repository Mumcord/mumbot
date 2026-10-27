//! `mumbot`: runs a headless Mumble bot. `webhook` runs the webhook bot; every other subcommand
//! drives the scripted test bot from the shell (Mumcord's E2E and smoke tests, manual checks).
//!
//! A test-bot run connects once, executes one subcommand (or, with `script`, one subcommand per
//! stdin line), optionally stays connected for `--hold` seconds and exits non-zero on the first
//! error. Mumcord's tests call this CLI by its old name `mumcord-bot`, so subcommands, flags,
//! output and exit codes must stay exactly as they are.

use std::io::BufRead;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use mumbot::samples::{PNG, Sample};
use mumbot::voice::{BotVoice, PlayOptions};
use mumbot::{Bot, BotConfig, MessageCheck, Moderation, UserCheck, env};
use mumcord_protocol::{DenyKind, permissions};

#[derive(Parser, Debug)]
#[command(
    name = "mumbot",
    version,
    about = "Headless bots for standard Mumble servers: the webhook bot and a scripted test bot"
)]
struct Cli {
    #[arg(long, default_value = env::HOST)]
    host: String,
    /// Server port (the local test server of this repository listens on 44738).
    #[arg(long, default_value_t = env::DEFAULT_PORT)]
    port: u16,
    #[arg(long, default_value = "Bot")]
    name: String,
    /// Persistent certificate name (defaults to --name); reuse it to keep the identity.
    #[arg(long)]
    cert: Option<String>,
    /// Log in as SuperUser (password: SUPERUSER_PASSWORD from the environment, else from .env in
    /// MUMBOT_TESTENV_DIR, MUMCORD_TESTENV_DIR or ./testenv).
    #[arg(long)]
    superuser: bool,
    /// Timeout in seconds for login and for each wait.
    #[arg(long, default_value_t = 10)]
    timeout: u64,
    /// Stay connected this many seconds after the command(s).
    #[arg(long, default_value_t = 0)]
    hold: u64,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the webhook bot: an HTTP API that posts messages into channels, see docs/webhook.md.
    /// Ignores the connection options above; the config file holds them.
    Webhook {
        /// TOML config file with the server settings and the API keys.
        #[arg(long, env = "MUMBOT_CONFIG")]
        config: PathBuf,
    },
    /// Connect and print session and certificate hash.
    Connect,
    /// Move into a channel (name or Root/Path).
    Join { channel: String },
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
    /// Wait for a condition; exits non-zero on timeout.
    #[command(subcommand)]
    Expect(Expect),
    /// Play a WAV file as voice into the current channel (terminator sent at the end).
    Play {
        file: PathBuf,
        /// Repeat until --seconds is over (forever without it).
        #[arg(long = "loop")]
        looping: bool,
        /// Stop after this many seconds.
        #[arg(long)]
        seconds: Option<f64>,
        /// Send this avatar position "x,y,z" with every packet (positional audio).
        #[arg(long, value_parser = parse_position, allow_hyphen_values = true)]
        position: Option<[f32; 3]>,
    },
    /// Announce a Mumble Link game context (like a game running next to the client), so people
    /// in the same game hear this bot's positional voice placed; an empty context means no game.
    GameContext {
        context: String,
        #[arg(long, default_value = "")]
        identity: String,
    },
    /// Record received voice for --seconds into a 48 kHz mono WAV (sum of all speakers, or
    /// only --from).
    Record {
        out: PathBuf,
        #[arg(long)]
        seconds: f64,
        #[arg(long)]
        from: Option<String>,
    },
    /// Wait until a user's voice reaches --min-rms dBFS (100 ms window); prints
    /// "<level dB> <latency ms>". Only audio after this command (or the first voice command of
    /// a script) is considered, so start it before the sender plays.
    ExpectAudio {
        #[arg(long)]
        from: Option<String>,
        #[arg(long, default_value_t = -40.0, allow_negative_numbers = true)]
        min_rms: f32,
        /// Seconds to wait (default: the global --timeout).
        #[arg(long)]
        timeout: Option<f64>,
    },
    /// Wait a number of seconds (pacing in scripts).
    Sleep { seconds: f64 },
    /// Run subcommands from stdin, one per line (shell-like quoting, # comments), on one
    /// connection; prints "ok <line>" after each.
    Script,
}

#[derive(Subcommand, Debug)]
enum Expect {
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

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum Toggle {
    On,
    Off,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Command::Webhook { config } = cli.command {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .init();
        let config = std::sync::Arc::new(mumbot::webhook::WebhookConfig::load(&config)?);
        return mumbot::webhook::run(config).await;
    }
    let mut bot = Bot::connect(BotConfig {
        host: cli.host.clone(),
        port: cli.port,
        name: cli.name.clone(),
        cert_name: cli.cert.clone(),
        data_dir: None,
        superuser: cli.superuser,
        password: None,
        tokens: Vec::new(),
        timeout: Duration::from_secs(cli.timeout),
    })
    .await?;
    let mut voice = None;
    match cli.command {
        Command::Script => run_script(&mut bot, &mut voice).await?,
        command => run(&mut bot, &mut voice, command).await?,
    }
    tokio::time::sleep(Duration::from_secs(cli.hold)).await;
    Ok(())
}

async fn run_script(bot: &mut Bot, voice: &mut Option<BotVoice>) -> Result<()> {
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
        run(bot, voice, parsed.command)
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

async fn run(bot: &mut Bot, voice: &mut Option<BotVoice>, command: Command) -> Result<()> {
    match command {
        Command::Webhook { .. } => bail!("the webhook bot runs its own loop"),
        Command::Connect => print_identity(bot)?,
        Command::Join { channel } => bot.join(&channel).await?,
        Command::Say { channel, html } => bot.send_channel(channel.as_deref(), &html)?,
        Command::SaySample { channel, sample } => {
            bot.send_channel(channel.as_deref(), &sample.html())?;
        }
        Command::Pm { user, html } => bot.send_private(&user, &html)?,
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
        Command::Mute { on } => bot.set_mute(matches!(on, Toggle::On))?,
        Command::Deaf { on } => bot.set_deaf(matches!(on, Toggle::On))?,
        Command::Moderate { user, mute, deaf } => {
            let on = |t: Option<Toggle>| t.map(|t| matches!(t, Toggle::On));
            bot.moderate(
                &user,
                Moderation {
                    mute: on(mute),
                    deaf: on(deaf),
                },
            )?;
        }
        Command::Move { user, channel } => bot.move_user(&user, &channel)?,
        Command::DenySpeak { channel } => bot.deny_speak(&channel).await?,
        Command::AllowSpeak { channel } => bot.allow_speak(&channel).await?,
        Command::DenyListen { channel } => bot.deny_listen(&channel).await?,
        Command::AllowListen { channel } => bot.allow_listen(&channel).await?,
        Command::Describe { channel, html } => bot.set_description(&channel, &html).await?,
        Command::GameContext { context, identity } => {
            // The official client prefixes the context with the plugin name ("Link\0").
            let context = if context.is_empty() {
                Vec::new()
            } else {
                [b"Link\0".as_slice(), context.as_bytes()].concat()
            };
            bot.handle().set_plugin_context(context, identity)?;
        }
        Command::Play {
            file,
            looping,
            seconds,
            position,
        } => {
            let opts = PlayOptions {
                forever: looping,
                max_duration: seconds.map(secs).transpose()?,
                position,
                ..Default::default()
            };
            let report = voice_of(bot, voice).await?.play_file(&file, opts).await?;
            println!(
                "played {} packets in {} ms",
                report.packets,
                report.finished.duration_since(report.started).as_millis()
            );
        }
        Command::Record { out, seconds, from } => {
            let v = voice_of(bot, voice).await?;
            let session = from.as_deref().map(|n| v.session_of(n)).transpose()?;
            v.record_to_wav(&out, session, secs(seconds)?).await?;
            println!("recorded {seconds} s to {}", out.display());
        }
        Command::ExpectAudio {
            from,
            min_rms,
            timeout,
        } => {
            let wait = match timeout {
                Some(t) => secs(t)?,
                None => bot.config().timeout,
            };
            let v = voice_of(bot, voice).await?;
            let hit = v
                .wait_audio(from.as_deref(), min_rms, std::time::Instant::now(), wait)
                .await?;
            println!("{:.1} {}", hit.level_db, hit.latency.as_millis());
        }
        Command::Expect(expect) => run_expect(bot, expect).await?,
        Command::Sleep { seconds } => {
            tokio::time::sleep(Duration::try_from_secs_f64(seconds).context("bad duration")?).await;
        }
        Command::Script => bail!("script is only valid as the top-level command"),
    }
    Ok(())
}

fn parse_position(text: &str) -> Result<[f32; 3]> {
    let parts = text
        .split(',')
        .map(|p| p.trim().parse::<f32>().context("not a number"))
        .collect::<Result<Vec<_>>>()?;
    <[f32; 3]>::try_from(parts)
        .ok()
        .context("give three numbers \"x,y,z\"")
}

fn secs(seconds: f64) -> Result<Duration> {
    Duration::try_from_secs_f64(seconds).context("bad duration")
}

/// Voice is started on first use and kept for the rest of the run.
async fn voice_of<'a>(bot: &Bot, voice: &'a mut Option<BotVoice>) -> Result<&'a BotVoice> {
    if voice.is_none() {
        *voice = Some(BotVoice::start(bot).await?);
    }
    voice.as_ref().context("voice not started")
}

async fn run_expect(bot: &Bot, expect: Expect) -> Result<()> {
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

fn as_strs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
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

    #[test]
    fn parses_positions() {
        assert_eq!(
            parse_position("-3, 0,1.5").expect("position"),
            [-3.0, 0.0, 1.5]
        );
        assert!(parse_position("1,2").is_err());
        assert!(parse_position("1,2,x").is_err());
        let line = ScriptLine::try_parse_from(["script", "play", "a.wav", "--position", "-3,0,0"])
            .expect("script line");
        assert!(matches!(
            line.command,
            Command::Play { position: Some([x, _, _]), .. } if x == -3.0
        ));
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
        let parse = |line: &str| {
            ScriptLine::try_parse_from(
                std::iter::once("script".to_owned()).chain(split_words(line).expect("split")),
            )
        };
        assert!(matches!(
            parse("expect user Alice --in Raid --muted true").map(|l| l.command),
            Ok(Command::Expect(Expect::User { .. }))
        ));
        assert!(parse("say-sample rich").is_ok());
        assert!(parse("nonsense").is_err());
    }

    #[test]
    fn cli_definition_is_valid() {
        <Cli as clap::CommandFactory>::command().debug_assert();
    }
}
