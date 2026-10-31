//! Music module: an HTTP API that plays MP3s from a media directory, uploads or allowed URLs as
//! voice into a channel, with a queue. Spec: `specs/05-music-specification.md`.
//!
//! The HTTP side ([`http`]) talks to the bot through [`MusicApi`] and to the [`Player`] state;
//! the player task decodes on a thread and sends voice through the shared connection.

mod config;
mod decode;
pub mod http;
mod player;
pub mod source;

pub use config::{DEFAULT_LISTEN, MusicConfig, UrlConfig};
pub use http::{MusicApi, serve};
pub use player::{Player, QueueView, TrackView, run};

use std::time::Duration;

use crate::checks::resolve_channel;
use crate::webhook::{Api, ApiKey, BotApi, ChannelRef};

impl MusicApi for BotApi {
    async fn connected(&self) -> bool {
        Api::connected(self).await
    }

    async fn uptime(&self) -> Duration {
        Api::uptime(self).await
    }

    async fn channel_exists(&self, id: u32) -> bool {
        Api::channel_exists(self, id).await
    }

    async fn allowed_ids(&self, key: &ApiKey) -> Vec<u32> {
        Api::allowed_ids(self, key).await
    }

    async fn resolve(&self, channel: &ChannelRef) -> Option<u32> {
        let handle = self.current_handle().await?;
        match channel {
            ChannelRef::Id(id) => Some(*id),
            ChannelRef::Name(name) => handle.with_state(|state| resolve_channel(state, name).ok()),
        }
    }

    async fn own_channel(&self) -> Option<u32> {
        let handle = self.current_handle().await?;
        handle.with_state(|state| state.own_user().map(|user| user.channel_id))
    }
}
