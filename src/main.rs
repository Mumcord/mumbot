//! `mumbot`: runs a headless Mumble bot. `webhook`, `peer` and `run` start the production modules
//! (`specs/03-modules-specification.md`); with the `tester` feature, every other subcommand
//! drives the scripted test bot from the shell (Mumcord's E2E and smoke tests, manual checks).
//! The main build answers those with a pointer to the tester build.
//!
//! A test-bot run connects once, executes one subcommand (or, with `script`, one subcommand per
//! stdin line), optionally stays connected for `--hold` seconds and exits non-zero on the first
//! error. Mumcord's tests call this CLI by its old name `mumcord-bot`, so subcommands, flags,
//! output and exit codes must stay exactly as they are. Each area of the test bot has its own
//! module in `cli/`; their subcommands are flattened into the top level for that compatibility.

#[cfg(feature = "tester")]
mod cli;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use mumbot::voice_settings::{VoiceConfig, VoiceSettings};
use mumbot::webhook::WebhookConfig;
use mumbot::{Bot, BotConfig, Module, env, modules};

#[derive(Parser, Debug)]
#[command(
    name = "mumbot",
    version,
    about = "Headless bots for standard Mumble servers: webhook and peer modules, and a scripted \
             test bot in the tester build"
)]
struct Cli {
    #[command(flatten)]
    connection: Connection,
    /// Stay connected this many seconds after the command(s).
    #[arg(long, default_value_t = 0)]
    hold: u64,
    #[command(flatten)]
    voice: VoiceArgs,
    #[command(subcommand)]
    command: Command,
}

/// Voice quality for the bot's voice senders, above the environment and the config file.
#[derive(Args, Debug)]
struct VoiceArgs {
    /// Opus bitrate in bit/s (6000 to 510000); the server's bandwidth limit can lower it.
    /// Environment: MUMBOT_VOICE_BITRATE.
    #[arg(long, global = true)]
    bitrate: Option<u32>,
    /// Voice packet length in ms (10 or 20). Environment: MUMBOT_VOICE_FRAME_MS.
    #[arg(long, global = true)]
    frame_ms: Option<u32>,
}

impl VoiceArgs {
    fn layer(&self) -> VoiceConfig {
        VoiceConfig {
            bitrate: self.bitrate,
            frame_ms: self.frame_ms,
        }
    }
}

/// Where and as whom a command-line run logs in; the modules read this from their config file.
#[derive(Args, Debug)]
struct Connection {
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
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the webhook module alone: an HTTP API that posts messages into channels, see
    /// docs/webhook.md. Ignores the connection options above; the config file holds them.
    Webhook {
        /// TOML config file with the server settings and the API keys.
        #[arg(long, env = "MUMBOT_CONFIG")]
        config: PathBuf,
    },
    /// Run the peer module alone: announces a mumpeer relay to Mumcord clients and hands out
    /// relay tokens, see docs/peer.md. Ignores the connection options above.
    Peer {
        /// TOML config file with the server settings and the `[peer]` section.
        #[arg(long, env = "MUMBOT_CONFIG")]
        config: PathBuf,
    },
    /// Run the music module alone: an HTTP API that plays MP3s as voice into a channel, see
    /// docs/music.md. Ignores the connection options above.
    Music {
        /// TOML config file with the server settings and the `[music]` section.
        #[arg(long, env = "MUMBOT_CONFIG")]
        config: PathBuf,
    },
    /// Run several modules on one connection, see docs/modules.md. Ignores the connection
    /// options above.
    Run {
        /// TOML config file with the server settings and one section per module.
        #[arg(long, env = "MUMBOT_CONFIG")]
        config: PathBuf,
        /// Modules to run, comma-separated; replaces the config's `[modules] enabled` list.
        #[arg(long, env = "MUMBOT_MODULES", value_delimiter = ',')]
        modules: Vec<Module>,
    },
    #[cfg(feature = "tester")]
    #[command(flatten)]
    Test(cli::Command),
    /// Self-register on the server under --name, with the connection options above. The peer
    /// module's setup needs this once.
    #[cfg(not(feature = "tester"))]
    Register,
    /// Any other command, answered with a pointer to the tester build.
    #[cfg(not(feature = "tester"))]
    #[command(external_subcommand)]
    Tester(Vec<String>),
}

/// The main build's answer to a test command (FR-3.9 of spec 03).
#[cfg(not(feature = "tester"))]
fn tester_only(command: &str) -> String {
    format!(
        "mumbot: `{command}` is not a command of this build. The test commands (say, expect, \
         play, script, …) are only in the tester build: the mumbot-tester release archive or \
         the ghcr.io/mumcord/mumbot-tester image."
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let (config, chosen) = match cli.command {
        Command::Webhook { config } => (config, vec![Module::Webhook]),
        Command::Peer { config } => (config, vec![Module::Peer]),
        Command::Music { config } => (config, vec![Module::Music]),
        Command::Run { config, modules } => (config, modules),
        #[cfg(feature = "tester")]
        Command::Test(command) => {
            let hold = Duration::from_secs(cli.hold);
            let settings = voice_settings(&cli.voice, None, VoiceConfig::default())?;
            let bot = connect(cli.connection).await?;
            return cli::run_top_level(bot, command, hold, settings).await;
        }
        #[cfg(not(feature = "tester"))]
        Command::Register => {
            let hold = Duration::from_secs(cli.hold);
            let bot = connect(cli.connection).await?;
            bot.register().await?;
            tokio::time::sleep(hold).await;
            return bot.disconnect().await;
        }
        #[cfg(not(feature = "tester"))]
        Command::Tester(args) => {
            eprintln!("{}", tester_only(args.first().map_or("", String::as_str)));
            std::process::exit(2);
        }
    };
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let config = Arc::new(WebhookConfig::load(&config, &chosen)?);
    // Checked now so a bad value stops the start, not the first voice a module sends.
    let music = config.music.as_ref().map(|music| music.voice());
    let settings = voice_settings(&cli.voice, music, config.voice)?;
    tracing::info!(
        bitrate_bps = settings.bitrate_bps,
        frame_ms = settings.frame_ms,
        "voice settings"
    );
    modules::run(config, settings).await
}

/// Command line, then environment, then the module's section, then the config's `[voice]`
/// section, then the defaults.
fn voice_settings(
    args: &VoiceArgs,
    module: Option<VoiceConfig>,
    voice: VoiceConfig,
) -> Result<VoiceSettings> {
    let env = VoiceConfig::from_env(|name| std::env::var(name).ok())?;
    VoiceSettings::resolve(args.layer(), env, module, voice)
}

async fn connect(cli: Connection) -> Result<Bot> {
    Bot::connect(BotConfig {
        host: cli.host,
        port: cli.port,
        name: cli.name,
        cert_name: cli.cert,
        data_dir: None,
        superuser: cli.superuser,
        password: None,
        tokens: Vec::new(),
        server_cert_sha256: None,
        timeout: Duration::from_secs(cli.timeout),
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        <Cli as clap::CommandFactory>::command().debug_assert();
    }

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("mumbot").chain(args.iter().copied()))
    }

    #[test]
    fn modules_are_chosen_by_command() {
        for args in [
            &["webhook", "--config", "bot.toml"][..],
            &["peer", "--config", "bot.toml"],
            &["music", "--config", "bot.toml"],
            &["run", "--config", "bot.toml"],
        ] {
            assert!(parse(args).is_ok(), "{args:?}");
        }
        let cli = parse(&[
            "run",
            "--config",
            "bot.toml",
            "--modules",
            "webhook,peer,music",
        ])
        .expect("run with modules");
        assert!(matches!(
            cli.command,
            Command::Run { modules, .. } if modules == [Module::Webhook, Module::Peer, Module::Music]
        ));
        assert!(parse(&["run", "--config", "bot.toml", "--modules", "radio"]).is_err());
    }

    #[cfg(not(feature = "tester"))]
    #[test]
    fn the_main_build_points_test_commands_to_the_tester_build() {
        let cli = parse(&["say", "--channel", "Lobby", "hi"]).expect("caught");
        let Command::Tester(args) = cli.command else {
            panic!("not caught: {:?}", cli.command);
        };
        assert!(tester_only(&args[0]).contains("`say`"));
        assert!(tester_only(&args[0]).contains("mumbot-tester"));
        assert!(matches!(
            parse(&["--name", "RelayBot", "register"])
                .expect("register")
                .command,
            Command::Register
        ));
    }

    #[cfg(feature = "tester")]
    #[test]
    fn test_subcommands_stay_at_the_top_level() {
        for args in [
            &["--port", "1", "connect"][..],
            &["say", "--channel", "Lobby", "<b>x</b>"],
            &["--superuser", "deny-listen", "Root/Raid"],
            &[
                "expect", "user", "Alice", "--in", "Raid", "--muted", "false",
            ],
            &[
                "expect-audio",
                "--from",
                "A",
                "--min-rms",
                "-50",
                "--timeout",
                "12",
            ],
            &[
                "play",
                "a.wav",
                "--loop",
                "--seconds",
                "2",
                "--position",
                "-3,0,0",
            ],
            &["webhook", "--config", "bot.toml"],
            &["peer", "--config", "bot.toml"],
        ] {
            assert!(parse(args).is_ok(), "{args:?}");
        }
        assert!(parse(&["test", "connect"]).is_err());
    }
}
