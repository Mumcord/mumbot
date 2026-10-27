//! Conditions over the observed server state, used by the `Bot::wait_*` helpers.

use anyhow::{Result, anyhow, bail};
use mumcord_protocol::{ServerState, TextMessage, User};

/// Expected state of another user; unset fields are not checked.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UserCheck {
    /// Channel the user is in (name or `Root/Path/Name`).
    pub in_channel: Option<String>,
}

impl UserCheck {
    pub fn matches(&self, user: &User, state: &ServerState) -> bool {
        self.in_channel
            .as_ref()
            .is_none_or(|name| resolve_channel(state, name).is_ok_and(|id| id == user.channel_id))
    }
}

/// Expected received text message; unset fields are not checked.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MessageCheck {
    /// Sender's user name.
    pub from: Option<String>,
    /// Message HTML contains this text.
    pub contains: Option<String>,
    /// Sent privately to this bot (true) or to a channel (false).
    pub private: Option<bool>,
    /// Sent to this channel.
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
    fn user_check_compares_the_channel() {
        let s = state();
        let user = find_user(&s, "alice").expect("user");
        let check = |channel: &str| UserCheck {
            in_channel: Some(channel.into()),
        };
        assert!(check("Lobby").matches(user, &s));
        assert!(!check("AFK").matches(user, &s));
        assert!(UserCheck::default().matches(user, &s));
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
