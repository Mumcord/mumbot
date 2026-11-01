//! The production modules of the bot and the runner that starts them on one Mumble connection.
//! Spec: `specs/03-modules-specification.md`.
//!
//! A module is a part of the bot that runs for as long as the bot does, next to the others: one
//! bot user, one certificate, one reconnect loop. A new module adds a variant here, its section
//! in the config file and its task in [`run`].

use std::future::pending;
use std::sync::Arc;

use anyhow::Result;
use serde::Deserialize;

use crate::history::{self, store::Store};
use crate::music::{self, Player};
use crate::peer::{self, HttpRelay};
use crate::plugins;
use crate::voice_settings::VoiceSettings;
use crate::webhook::{self, BotApi, WebhookConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Module {
    /// HTTP API that posts messages into channels (`docs/webhook.md`).
    Webhook,
    /// Announces a mumpeer relay to Mumcord clients and hands out relay tokens (`docs/peer.md`).
    Peer,
    /// HTTP API that plays MP3s as voice into a channel, with a queue (`docs/music.md`).
    Music,
    /// Records channel chat and answers Mumcord's history sync (`docs/history.md`).
    History,
    /// Announces the Mumcord plugins the server community uses (`docs/plugins.md`).
    Plugins,
}

/// Logs in once and runs the config's enabled modules until the process is stopped or one of
/// them fails. The config must come from [`WebhookConfig::load`], which validated it for them.
/// `music_voice` is the music module's resolved voice settings.
pub async fn run(config: Arc<WebhookConfig>, music_voice: VoiceSettings) -> Result<()> {
    let enabled = &config.modules.enabled;
    let listener = if enabled.contains(&Module::Webhook) {
        if !config.listens_locally() {
            tracing::warn!(
                listen = %config.http.listen,
                "listening outside loopback over plain HTTP: the API key travels in clear text, \
                 put a TLS proxy in front"
            );
        }
        // Bound before the login, so a taken port fails fast.
        let listener = tokio::net::TcpListener::bind(config.listen_addr()?).await?;
        tracing::info!(
            listen = %listener.local_addr()?,
            keys = config.keys.len(),
            "webhook module listening"
        );
        Some(listener)
    } else {
        None
    };
    let relay = match (&config.peer, enabled.contains(&Module::Peer)) {
        (Some(peer), true) => {
            if !peer.url()?.is_local_or_tls() {
                tracing::warn!(
                    api_url = %peer.api_url,
                    "peer: allow_insecure_http is set, the API secret travels in clear text to \
                     another host; use https with api_fp"
                );
            }
            Some((
                HttpRelay::new(peer, peer.load_secret()?)?,
                peer::Rules::from(peer),
            ))
        }
        _ => None,
    };

    let music_listener = match (&config.music, enabled.contains(&Module::Music)) {
        (Some(music), true) => {
            let addr = music.listen_addr()?;
            if !addr.ip().is_loopback() {
                tracing::warn!(
                    listen = %addr,
                    "music: listening outside loopback over plain HTTP: the API key travels in \
                     clear text, put a TLS proxy in front"
                );
            }
            let listener = tokio::net::TcpListener::bind(addr).await?;
            tracing::info!(
                listen = %listener.local_addr()?,
                keys = music.keys.len(),
                "music module listening"
            );
            Some((listener, Arc::new(music.clone())))
        }
        _ => None,
    };

    let history = match (&config.history, enabled.contains(&Module::History)) {
        (Some(history), true) => {
            let path = history.database_path(config.server.data_dir.as_deref());
            let store = tokio::task::spawn_blocking(move || Store::open(&path)).await??;
            Some((history.clone(), store))
        }
        _ => None,
    };

    let announcement = match (&config.plugins, enabled.contains(&Module::Plugins)) {
        (Some(plugins_config), true) => {
            let suggestions = plugins_config.suggestions();
            let path = plugins::config::seq_path(config.server.data_dir.as_deref());
            let message = tokio::task::spawn_blocking(move || {
                plugins::config::current_seq(&path, &suggestions)
                    .map(|seq| plugins::wire::encode(seq, &suggestions))
            })
            .await??;
            Some(message)
        }
        _ => None,
    };

    let api = Arc::new(BotApi::new(Arc::clone(&config)));
    // Subscribed before the login: Mumcord clients greet a joining bot right away, and the
    // history module must see everyone arrive.
    let events = api.subscribe();
    let history_events = api.subscribe();
    let plugins_events = api.subscribe();
    let music_events = api.subscribe();
    api.connect().await?;
    tracing::info!(name = %config.server.name, modules = ?enabled, "bot connected");

    let webhook = async {
        match listener {
            Some(listener) => webhook::serve(listener, Arc::clone(&config), Arc::clone(&api)).await,
            None => pending().await,
        }
    };
    let peer = async {
        match relay {
            Some((relay, rules)) => peer::run(relay, Arc::clone(&api), rules, events).await,
            None => pending().await,
        }
    };
    let music = async {
        match music_listener {
            Some((listener, music)) => {
                let player = Arc::new(Player::new(&music));
                let server = config.server.host.clone();
                tokio::select! {
                    result = music::serve(listener, music, server, Arc::clone(&api), Arc::clone(&player)) => result,
                    result = music::run(player, Arc::clone(&api), Arc::clone(&config), music_voice) => result,
                    result = music::announce::run(Arc::clone(&api), music_events) => result,
                }
            }
            None => pending().await,
        }
    };
    let history = async {
        match history {
            Some((history, store)) => {
                history::run(history, store, Arc::clone(&api), history_events).await
            }
            None => pending().await,
        }
    };
    let plugins = async {
        match announcement {
            Some(message) => plugins::run(Arc::clone(&api), message, plugins_events).await,
            None => pending().await,
        }
    };
    tokio::select! {
        result = webhook => result,
        result = peer => result,
        result = music => result,
        result = history => result,
        result = plugins => result,
        result = webhook::supervise(Arc::clone(&api)) => result,
    }
}
