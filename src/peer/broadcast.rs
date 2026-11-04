//! Broadcasts the bot set up at the relay: screen sharing (Mumcord spec 07, FR-7.40 to FR-7.50)
//! and plugin data (Mumcord spec 09, FR-9.88). The table keeps who publishes each one, what it
//! carries and which sessions it admitted, so the bot can check `bcast-admit` and `bcast-kick`
//! against the publisher and clean up at the relay when people leave.
//!
//! Memory only, like the relay's own broadcast table: after a bot restart or reconnect the
//! relay's idle timeout ends broadcasts nobody publishes to any more.

use std::collections::HashMap;

use mumcord_p2p::{BroadcastId, RelayUse};

/// One broadcast at the relay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broadcast {
    /// The session that asked for it; only it may admit and kick.
    pub publisher: u32,
    pub publisher_cert: String,
    /// `ScreenShare` or `PluginData`.
    pub kind: RelayUse,
    /// Admitted sessions with the certificate hash the relay knows them by.
    pub viewers: HashMap<u32, String>,
}

/// A call the relay needs after a change on the Mumble server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cleanup {
    Delete(BroadcastId),
    RemoveViewer(BroadcastId, String),
}

/// All broadcasts the bot created on this connection.
#[derive(Debug, Default)]
pub struct Broadcasts {
    by_id: HashMap<BroadcastId, Broadcast>,
}

impl Broadcasts {
    /// Records a new broadcast of `publisher` and returns the one it replaces: a session shares
    /// one screen and sends one plugin data broadcast at a time, so an older broadcast of the
    /// same session and kind is finished.
    pub fn insert(
        &mut self,
        id: BroadcastId,
        publisher: u32,
        cert: String,
        kind: RelayUse,
    ) -> Option<BroadcastId> {
        let old = self.of_publisher(publisher, kind);
        if let Some(old) = old {
            self.by_id.remove(&old);
        }
        self.by_id.insert(
            id,
            Broadcast {
                publisher,
                publisher_cert: cert,
                kind,
                viewers: HashMap::new(),
            },
        );
        old
    }

    pub fn of_publisher(&self, session: u32, kind: RelayUse) -> Option<BroadcastId> {
        self.by_id
            .iter()
            .find(|(_, b)| b.publisher == session && b.kind == kind)
            .map(|(id, _)| *id)
    }

    /// The sessions other than `except` that publish a broadcast of `kind` in `channel`.
    /// `channel_of` is `None` for sessions not on the server.
    pub fn publishers_in(
        &self,
        kind: RelayUse,
        channel: u32,
        except: u32,
        channel_of: impl Fn(u32) -> Option<u32>,
    ) -> usize {
        self.by_id
            .values()
            .filter(|b| {
                b.kind == kind && b.publisher != except && channel_of(b.publisher) == Some(channel)
            })
            .count()
    }

    /// Whether `session` publishes or watches any broadcast.
    pub fn involves(&self, session: u32) -> bool {
        self.by_id
            .values()
            .any(|b| b.publisher == session || b.viewers.contains_key(&session))
    }

    pub fn get(&self, id: BroadcastId) -> Option<&Broadcast> {
        self.by_id.get(&id)
    }

    pub fn remove(&mut self, id: BroadcastId) -> Option<Broadcast> {
        self.by_id.remove(&id)
    }

    pub fn admit(&mut self, id: BroadcastId, viewer: u32, cert: String) {
        if let Some(broadcast) = self.by_id.get_mut(&id) {
            broadcast.viewers.insert(viewer, cert);
        }
    }

    /// Removes `viewer` from broadcast `id` and returns its certificate hash. The relay knows
    /// viewers by certificate, so every session with that certificate goes with it.
    pub fn kick(&mut self, id: BroadcastId, viewer: u32) -> Option<String> {
        let broadcast = self.by_id.get_mut(&id)?;
        let cert = broadcast.viewers.remove(&viewer)?;
        broadcast.viewers.retain(|_, other| *other != cert);
        Some(cert)
    }

    /// `session` left the server or changed its certificate: its broadcasts end, and it leaves
    /// the ones it watched, unless another admitted session holds the same certificate (a
    /// second device of the same user keeps watching).
    pub fn left(&mut self, session: u32) -> Vec<Cleanup> {
        let mut cleanup: Vec<Cleanup> = Vec::new();
        self.by_id.retain(|id, broadcast| {
            if broadcast.publisher == session {
                cleanup.push(Cleanup::Delete(*id));
                return false;
            }
            if let Some(cert) = broadcast.viewers.remove(&session)
                && !broadcast.viewers.values().any(|other| *other == cert)
            {
                cleanup.push(Cleanup::RemoveViewer(*id, cert));
            }
            true
        });
        cleanup
    }

    /// Removes every admitted viewer who is no longer in its publisher's channel (FR-7.42 holds
    /// for the whole broadcast, not only at admission). `channel_of` is `None` for sessions not
    /// on the server.
    pub fn outside_channel(&mut self, channel_of: impl Fn(u32) -> Option<u32>) -> Vec<Cleanup> {
        let mut cleanup = Vec::new();
        for (id, broadcast) in &mut self.by_id {
            let Some(channel) = channel_of(broadcast.publisher) else {
                continue;
            };
            let gone: Vec<u32> = broadcast
                .viewers
                .keys()
                .copied()
                .filter(|viewer| channel_of(*viewer).is_some_and(|c| c != channel))
                .collect();
            for viewer in gone {
                if let Some(cert) = broadcast.viewers.remove(&viewer)
                    && !broadcast.viewers.values().any(|other| *other == cert)
                {
                    cleanup.push(Cleanup::RemoveViewer(*id, cert));
                }
            }
        }
        cleanup
    }

    pub fn clear(&mut self) {
        self.by_id.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: BroadcastId = BroadcastId(1);
    const B: BroadcastId = BroadcastId(2);

    const SCR: RelayUse = RelayUse::ScreenShare;
    const PLG: RelayUse = RelayUse::PluginData;

    fn table() -> Broadcasts {
        let mut table = Broadcasts::default();
        assert_eq!(table.insert(A, 10, "p".into(), SCR), None);
        table.admit(A, 20, "v1".into());
        table.admit(A, 21, "v1".into());
        table.admit(A, 30, "v2".into());
        table
    }

    #[test]
    fn a_new_broadcast_of_the_same_session_replaces_the_old_one() {
        let mut table = table();
        assert_eq!(table.insert(B, 10, "p".into(), SCR), Some(A));
        assert_eq!(table.get(A), None);
        assert_eq!(table.of_publisher(10, SCR), Some(B));
    }

    #[test]
    fn a_session_has_one_broadcast_per_kind() {
        let mut table = table();
        assert_eq!(table.insert(B, 10, "p".into(), PLG), None);
        assert_eq!(table.of_publisher(10, SCR), Some(A));
        assert_eq!(table.of_publisher(10, PLG), Some(B));
        assert_eq!(table.insert(BroadcastId(3), 10, "p".into(), PLG), Some(B));
        assert_eq!(table.of_publisher(10, SCR), Some(A));
    }

    #[test]
    fn publishers_are_counted_per_kind_and_channel() {
        let mut table = table();
        table.insert(B, 11, "q".into(), PLG);
        table.insert(BroadcastId(3), 12, "r".into(), PLG);
        table.insert(BroadcastId(4), 13, "s".into(), PLG);
        let channels = HashMap::from([(10, 1), (11, 1), (12, 1), (13, 2)]);
        let channel_of = |s: u32| channels.get(&s).copied();
        assert_eq!(table.publishers_in(PLG, 1, 0, channel_of), 2);
        assert_eq!(table.publishers_in(PLG, 1, 11, channel_of), 1);
        assert_eq!(table.publishers_in(SCR, 1, 0, channel_of), 1);
        assert_eq!(table.publishers_in(PLG, 2, 0, channel_of), 1);
    }

    #[test]
    fn a_kick_removes_every_session_of_that_certificate() {
        let mut table = table();
        assert_eq!(table.kick(A, 20), Some("v1".into()));
        let viewers = &table.get(A).expect("broadcast").viewers;
        assert_eq!(viewers.keys().copied().collect::<Vec<_>>(), vec![30]);
        assert_eq!(table.kick(A, 20), None);
        assert_eq!(table.kick(B, 30), None);
    }

    #[test]
    fn leaving_ends_own_broadcasts_and_watching() {
        let mut table = table();
        // The second device of v1 still watches.
        assert_eq!(table.left(20), vec![]);
        assert_eq!(table.left(21), vec![Cleanup::RemoveViewer(A, "v1".into())]);
        assert_eq!(table.left(10), vec![Cleanup::Delete(A)]);
        assert_eq!(table.get(A), None);
    }

    #[test]
    fn viewers_outside_the_publishers_channel_are_removed() {
        let mut table = table();
        let channels = HashMap::from([(10, 1), (20, 1), (21, 2), (30, 2)]);
        let mut cleanup = table.outside_channel(|s| channels.get(&s).copied());
        // 21 left, but 20 still watches with the same certificate, so the relay keeps it.
        assert_eq!(cleanup, vec![Cleanup::RemoveViewer(A, "v2".into())]);
        let viewers = &table.get(A).expect("broadcast").viewers;
        assert_eq!(viewers.keys().copied().collect::<Vec<_>>(), vec![20]);
        // The publisher moves on without 20.
        let channels = HashMap::from([(10, 2), (20, 1)]);
        cleanup = table.outside_channel(|s| channels.get(&s).copied());
        assert_eq!(cleanup, vec![Cleanup::RemoveViewer(A, "v1".into())]);
        // A session missing from the server is left to the leave handling.
        table.admit(A, 30, "v2".into());
        let channels = HashMap::from([(10, 3)]);
        assert_eq!(table.outside_channel(|s| channels.get(&s).copied()), vec![]);
    }
}
