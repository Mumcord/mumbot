//! Group membership for `allowed_groups` (issue #31), read from the server's answer to an ACL
//! query (`Handle::query_acl`, answered as `Event::Acl`).
//!
//! Standard servers answer an ACL query only for clients with the Write permission on that
//! channel, so the bot needs it there; without it the bot refuses to run rather than fall back to
//! allowing everyone. Group members are registered users, identified by their user id.
//!
//! The server never tells clients that a group's members changed, so membership is read again for
//! every token request (at most one query per `MIN_READ_INTERVAL`, so a burst of requests cannot
//! flood the server) and periodically per `READ_INTERVAL`. The periodic read is what drops people
//! who lost access at the relay.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::time::Duration;

use mumcord_protocol::proto;

/// Shortest time between two ACL queries.
pub const MIN_READ_INTERVAL: Duration = Duration::from_secs(10);
/// Groups are read again at least this often, also without any token request.
pub const READ_INTERVAL: Duration = Duration::from_secs(60);
/// How long the bot waits for the answer to one query.
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
/// Group names come from the server and are untrusted, so they are capped.
const MAX_NAME_CHARS: usize = 100;

/// Members per group name, as the server listed them for the queried channel.
pub type Members = HashMap<String, HashSet<u32>>;

/// Why a group read failed. The bot never grants tokens on a failed read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryError {
    /// The bot has no Write permission on that channel, which standard servers require to answer.
    NoPermission,
    /// The server did not answer within `QUERY_TIMEOUT`.
    Timeout,
    /// The bot is not connected.
    NotConnected,
    /// The configured channel does not exist on the server.
    UnknownChannel,
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QueryError::NoPermission => f.write_str("no Write permission on the group channel"),
            QueryError::Timeout => f.write_str("the server did not answer the ACL query"),
            QueryError::NotConnected => f.write_str("not connected"),
            QueryError::UnknownChannel => f.write_str("the group channel does not exist"),
        }
    }
}

impl std::error::Error for QueryError {}

/// Resolves one answer: the members a group inherited plus the users it added in this channel,
/// minus the users it removes in it. Groups without a name are ignored.
pub fn members(acl: &proto::Acl) -> Members {
    let mut out: Members = HashMap::new();
    for group in &acl.groups {
        let name = group.name.trim();
        if name.is_empty() || name.len() > MAX_NAME_CHARS {
            continue;
        }
        if name.is_empty() || name.len() > MAX_NAME_CHARS {
            continue;
        }
        let entry = out.entry(name.to_owned()).or_default();
        for id in group.inherited_members.iter().chain(group.add.iter()) {
            entry.insert(*id);
        }
        for id in &group.remove {
            entry.remove(id);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(
        name: &str,
        inherited: &[u32],
        added: &[u32],
        removed: &[u32],
    ) -> proto::acl::ChanGroup {
        proto::acl::ChanGroup {
            name: name.to_owned(),
            inherited: Some(true),
            inherit: Some(true),
            inheritable: Some(true),
            add: added.to_vec(),
            remove: removed.to_vec(),
            inherited_members: inherited.to_vec(),
        }
    }

    #[test]
    fn members_are_inherited_plus_added_minus_removed() {
        let acl = proto::Acl {
            channel_id: 2,
            groups: vec![
                group("Relay", &[10, 11], &[12], &[11]),
                group("", &[10], &[], &[]),
            ],
            acls: Vec::new(),
            ..Default::default()
        };
        let read = members(&acl);
        assert_eq!(read.get("Relay"), Some(&HashSet::from([10, 12])));
        assert_eq!(read.len(), 1, "a group without a name is ignored");
    }

    #[test]
    fn long_group_names_are_ignored() {
        let long = "n".repeat(MAX_NAME_CHARS + 1);
        let acl = proto::Acl {
            groups: vec![group(&long, &[10], &[], &[])],
            ..Default::default()
        };
        assert!(!members(&acl).contains_key(&long));
    }
}
