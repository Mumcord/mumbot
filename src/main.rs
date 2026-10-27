//! `mumbot`: runs a headless Mumble bot. `webhook` runs the webhook bot; every other subcommand
//! drives the scripted test bot from the shell (Mumcord's E2E and smoke tests, manual checks).
//!
//! A test-bot run connects once, executes one subcommand (or, with `script`, one subcommand per
//! stdin line), optionally stays connected for `--hold` seconds and exits non-zero on the first
//! error. Mumcord's tests call this CLI by its old name `mumcord-bot`, so subcommands, flags,
//! output and exit codes must stay exactly as they are. Each area of the test bot has its own
//! module in `cli/`; their subcommands are flattened into the top level for that compatibility.

mod cli;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use mumbot::webhook::{self, WebhookConfig};
use mumbot::{Bot, BotConfig, env};

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
    #[command(flatten)]
    Test(cli::Command),
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let command = match cli.command {
        Command::Webhook { config } => {
            tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .init();
            let config = Arc::new(WebhookConfig::load(&config)?);
            return webhook::run(config).await;
        }
        Command::Test(command) => command,
    };
    let bot = Bot::connect(BotConfig {
        host: cli.host,
        port: cli.port,
        name: cli.name,
        cert_name: cli.cert,
        data_dir: None,
        superuser: cli.superuser,
        password: None,
        tokens: Vec::new(),
        timeout: Duration::from_secs(cli.timeout),
    })
    .await?;
    cli::run_top_level(bot, command, Duration::from_secs(cli.hold)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        <Cli as clap::CommandFactory>::command().debug_assert();
    }

    #[test]
    fn test_subcommands_stay_at_the_top_level() {
        let parse = |args: &[&str]| {
            Cli::try_parse_from(std::iter::once("mumbot").chain(args.iter().copied()))
        };
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
        ] {
            assert!(parse(args).is_ok(), "{args:?}");
        }
        assert!(parse(&["test", "connect"]).is_err());
    }
}
