//! Mumbot: headless bots that are ordinary Mumble clients built on `mumcord-protocol`, so they
//! work against any unmodified standard Mumble server.
//!
//! [`Bot`] is the shared core (connect, persistent certificate, waiting on server state). Built on
//! it: the [`webhook`] bot with the optional [`peer`] module, and the scripted test bot behind the `mumbot` CLI subcommands (chat,
//! state, ACL helpers, `expect` checks) with voice in [`voice`], used by Mumcord's E2E and smoke
//! tests.

mod acl;
mod bot;
pub mod cert;
pub mod checks;
pub mod env;
pub mod peer;
pub mod samples;
pub mod voice;
pub mod webhook;

pub use bot::{Bot, BotConfig, DEFAULT_TIMEOUT, Moderation};
pub use checks::{MessageCheck, UserCheck};
pub use mumcord_protocol as protocol;
