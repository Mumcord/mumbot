//! Webhook API keys: a headless bot that turns one HTTP request into one channel message.
//!
//! The bot is an ordinary Mumble client (`Bot`); the HTTP listener runs on our side, so nothing
//! is added to or required by the Mumble server. Spec: `specs/01-webhook-specification.md`.

mod config;
pub mod http;
pub mod markdown;
mod state;

pub use config::{ApiKey, ChannelRef, ChannelToken, HttpConfig, ServerConfig, WebhookConfig};
pub use http::{Api, DeliveryError, RateLimiter, Request, Response, handle, parse_request, serve};
pub use markdown::render_html;
pub use state::{BotApi, run, supervise};

use std::time::Duration;

/// Message cap of the webhook API (Q2.7 of the spec).
pub const MAX_MESSAGE_CHARS: usize = 5000;
/// Body cap: 5000 characters can encode to more bytes, so the practical byte limit is 20 KB.
pub const MAX_BODY_BYTES: usize = 20_000;
/// How long the handler waits for the server to accept a message (Q2.10).
pub const DELIVERY_TIMEOUT: Duration = Duration::from_secs(3);
/// Requests per minute per key (Q2.9).
pub const RATE_LIMIT_PER_MINUTE: u32 = 20;
