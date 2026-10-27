//! Mumbot: headless bots that are ordinary Mumble clients built on `mumcord-protocol`, so they
//! work against any unmodified standard Mumble server.
//!
//! [`Bot`] is the shared core (connect, persistent certificate, waiting on server state);
//! [`webhook`] is the webhook bot built on it.

mod acl;
mod bot;
pub mod cert;
pub mod checks;
pub mod env;
pub mod webhook;

pub use bot::{Bot, BotConfig, DEFAULT_TIMEOUT};
pub use checks::{MessageCheck, UserCheck};
pub use mumcord_protocol as protocol;
