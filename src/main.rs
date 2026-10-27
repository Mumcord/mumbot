//! `mumbot`: runs a headless Mumble bot.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};
use mumbot::webhook::{self, WebhookConfig};

#[derive(Parser, Debug)]
#[command(
    name = "mumbot",
    version,
    about = "Headless bots for standard Mumble servers"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the webhook bot: an HTTP API that posts messages into channels, see docs/webhook.md.
    Webhook {
        /// TOML config file with the server settings and the API keys.
        #[arg(long, env = "MUMBOT_CONFIG")]
        config: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Webhook { config } => {
            tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .init();
            let config = Arc::new(WebhookConfig::load(&config)?);
            webhook::run(config).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        <Cli as clap::CommandFactory>::command().debug_assert();
    }
}
