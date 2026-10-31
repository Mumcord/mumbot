//! Mumbot: headless bots that are ordinary Mumble clients built on `mumcord-protocol`, so they
//! work against any unmodified standard Mumble server.
//!
//! [`Bot`] is the shared core (connect, persistent certificate, waiting on server state). Built on
//! it: the production [`modules`] ([`webhook`] and [`peer`]) that share one connection, and, with
//! the `tester` feature, the scripted test bot behind the `mumbot` CLI subcommands (chat, state,
//! ACL helpers, `expect` checks) with voice in `voice`, used by Mumcord's E2E and smoke tests.

#[cfg(feature = "tester")]
mod acl;
mod bot;
pub mod cert;
pub mod checks;
pub mod env;
pub mod modules;
pub mod peer;
#[cfg(feature = "tester")]
pub mod samples;
#[cfg(feature = "tester")]
pub mod voice;
pub mod webhook;

#[cfg(feature = "tester")]
pub use bot::Moderation;
pub use bot::{Bot, BotConfig, DEFAULT_TIMEOUT};
#[cfg(feature = "tester")]
pub use checks::{MessageCheck, UserCheck};
pub use modules::Module;
pub use mumcord_protocol as protocol;
