//! Conditions over the observed server state, used by the `Bot::wait_*` helpers and the CLI's
//! `expect` subcommand.

use anyhow::{Result, anyhow, bail};
use mumcord_protocol::{ServerState, TextMessage, User};

/// Expected state of another user; unset fields are not checked.
#[derive(Debug, Clone, Default, PartialEq, clap::Args)]
pub struct UserCheck {
    /// Channel the user is in (name or `Root/Path/Name`).
    #[arg(long = "in")]
    pub in_channel: Option<String>,
    /// Muted (self or by an admin).
    #[arg(long)]
    pub muted: Option<bool>,
    /// Deafened (self or by an admin).
    #[arg(long)]
    pub deaf: Option<bool>,
    /// Channel the user listens to (ear icon).
    #[arg(long)]
    pub listening: Option<String>,
    /// Channel the user does not listen to.
    #[arg(long)]
    pub not_listening: Option<String>,
    /// Registered on the server.
    #[arg(long)]
    pub registered: Option<bool>,
    /// Comment contains this text.
    #[arg(long)]
    pub comment_contains: Option<String>,
    /// Has an avatar.
    #[arg(long)]
    pub has_avatar: Option<bool>,
    /// Avatar equals these bytes (library use; the CLI fills it from `--avatar-file`).
    #[arg(skip)]
    pub avatar: Option<Vec<u8>>,
    /// Certificate hash (SHA-1 hex).
    #[arg(long)]
    pub cert_hash: Option<String>,
}

impl UserCheck {
    pub fn matches(&self, user: &User, state: &ServerState) -> bool {
        let in_channel = |name: &Option<String>| {
            name.as_ref()
                .map(|n| resolve_channel(state, n).is_ok_and(|id| id == user.channel_id))
        };
        let listens = |name: &Option<String>| {
            name.as_ref()
                .map(|n| resolve_channel(state, n).is_ok_and(|id| user.listening.contains(&id)))
        };
        in_channel(&self.in_channel).unwrap_or(true)
            && self
                .muted
                .is_none_or(|m| (user.mute || user.self_mute) == m)
            && self.deaf.is_none_or(|d| (user.deaf || user.self_deaf) == d)
            && listens(&self.listening).unwrap_or(true)
            && listens(&self.not_listening).is_none_or(|l| !l)
            && self.registered.is_none_or(|r| user.user_id.is_some() == r)
            && self.comment_contains.as_ref().is_none_or(|text| {
                user.comment
                    .as_ref()
                    .is_some_and(|c| c.contains(text.as_str()))
            })
            && self
                .has_avatar
                .is_none_or(|a| (user.texture.is_some() || user.texture_hash.is_some()) == a)
            && self
                .avatar
                .as_ref()
                .is_none_or(|bytes| user.texture.as_ref() == Some(bytes))
            && self
                .cert_hash
                .as_ref()
                .is_none_or(|h| user.cert_hash.as_ref() == Some(h))
    }
}

/// Expected received text message; unset fields are not checked.
#[derive(Debug, Clone, Default, PartialEq, clap::Args)]
pub struct MessageCheck {
    /// Sender's user name.
    #[arg(long)]
    pub from: Option<String>,
    /// Message HTML contains this text.
    #[arg(long)]
    pub contains: Option<String>,
    /// Sent privately to this bot (true) or to a channel (false).
    #[arg(long)]
    pub private: Option<bool>,
    /// Sent to this channel.
    #[arg(long)]
    pub channel: Option<String>,
}

impl MessageCheck {
    pub fn matches(&self, msg: &TextMessage, state: &ServerState) -> bool {
        let sender = msg.actor.and_then(|s| state.users.get(&s));
        self.from
            .as_ref()
            .is_none_or(|name| sender.is_some_and(|u| &u.name == name))
            && self
                .contains
                .as_ref()
                .is_none_or(|text| msg.message.contains(text.as_str()))
            && self
                .private
                .is_none_or(|p| !msg.target.sessions.is_empty() == p)
            && self.channel.as_ref().is_none_or(|name| {
                resolve_channel(state, name).is_ok_and(|id| msg.target.channels.contains(&id))
            })
    }
}

/// Finds a channel by name (must be unique) or by path from the root (`Root/Raid`, `/Raid`).
pub fn resolve_channel(state: &ServerState, name: &str) -> Result<u32> {
    if name.contains('/') {
        let mut parts = name.split('/').filter(|p| !p.is_empty()).peekable();
        let mut id = 0;
        // The root's real name depends on the server's configuration; "Root" always works.
        let root_name = state.channels.get(&0).map(|c| c.name.as_str());
        if parts
            .peek()
            .is_some_and(|p| *p == "Root" || Some(*p) == root_name)
        {
            parts.next();
        }
        for part in parts {
            id = state
                .child_by_name(id, part)
                .ok_or_else(|| anyhow!("no channel {name:?}"))?
                .id;
        }
        return Ok(id);
    }
    let mut found = state.channels.values().filter(|c| c.name == name);
    match (found.next(), found.next()) {
        (Some(c), None) => Ok(c.id),
        (None, _) => bail!("no channel {name:?}"),
        (Some(_), Some(_)) => bail!("channel name {name:?} is ambiguous; use a path like Root/…"),
    }
}

/// Finds a connected user by name.
pub fn find_user<'a>(state: &'a ServerState, name: &str) -> Option<&'a User> {
    state.users.values().find(|u| u.name == name)
}

#[cfg(test)]
mod tests {
    use mumcord_protocol::{Channel, TextTarget};

    use super::*;

    fn state() -> ServerState {
        let mut s = ServerState::default();
        for (id, parent, name) in [
            (0, None, "Test Server"),
            (1, Some(0), "Lobby"),
            (2, Some(0), "Raid"),
            (3, Some(1), "Raid"),
            (4, Some(0), "AFK"),
        ] {
            s.channels.insert(
                id,
                Channel {
                    id,
                    parent,
                    name: name.to_owned(),
                    ..Default::default()
                },
            );
        }
        s.users.insert(
            7,
            User {
                session: 7,
                name: "alice".to_owned(),
                channel_id: 1,
                self_mute: true,
                user_id: Some(3),
                listening: [4].into(),
                comment: Some("hello <b>world</b>".to_owned()),
                ..Default::default()
            },
        );
        s
    }

    #[test]
    fn resolves_names_and_paths() {
        let s = state();
        assert_eq!(resolve_channel(&s, "Lobby").ok(), Some(1));
        assert!(resolve_channel(&s, "Raid").is_err(), "ambiguous");
        assert_eq!(resolve_channel(&s, "Root/Raid").ok(), Some(2));
        assert_eq!(resolve_channel(&s, "/Lobby/Raid").ok(), Some(3));
        assert_eq!(resolve_channel(&s, "Test Server/Lobby").ok(), Some(1));
        assert!(resolve_channel(&s, "Nope").is_err());
    }

    #[test]
    fn user_check_compares_each_field() {
        let s = state();
        let user = find_user(&s, "alice").expect("user");
        let ok = UserCheck {
            in_channel: Some("Lobby".into()),
            muted: Some(true),
            deaf: Some(false),
            listening: Some("AFK".into()),
            not_listening: Some("Root/Raid".into()),
            registered: Some(true),
            comment_contains: Some("<b>world".into()),
            has_avatar: Some(false),
            ..Default::default()
        };
        assert!(ok.matches(user, &s));
        for bad in [
            UserCheck {
                in_channel: Some("AFK".into()),
                ..Default::default()
            },
            UserCheck {
                muted: Some(false),
                ..Default::default()
            },
            UserCheck {
                listening: Some("Lobby".into()),
                ..Default::default()
            },
            UserCheck {
                not_listening: Some("AFK".into()),
                ..Default::default()
            },
            UserCheck {
                registered: Some(false),
                ..Default::default()
            },
            UserCheck {
                comment_contains: Some("bye".into()),
                ..Default::default()
            },
            UserCheck {
                avatar: Some(vec![1]),
                ..Default::default()
            },
        ] {
            assert!(!bad.matches(user, &s), "{bad:?}");
        }
    }

    #[test]
    fn message_check_filters_sender_target_and_text() {
        let s = state();
        let msg = TextMessage {
            actor: Some(7),
            target: TextTarget::channel(1),
            message: "<b>hi</b>".to_owned(),
        };
        let ok = MessageCheck {
            from: Some("alice".into()),
            contains: Some("<b>".into()),
            private: Some(false),
            channel: Some("Lobby".into()),
        };
        assert!(ok.matches(&msg, &s));
        assert!(
            !MessageCheck {
                private: Some(true),
                ..Default::default()
            }
            .matches(&msg, &s)
        );
        assert!(
            !MessageCheck {
                from: Some("bob".into()),
                ..Default::default()
            }
            .matches(&msg, &s)
        );
    }
}
