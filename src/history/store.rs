//! The local message store: one SQLite file with the recorded channel messages.
//!
//! Every stored field is listed in `docs/history.md`. The file is created owner-only on Unix,
//! because it holds chat contents.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

/// Schema version in `PRAGMA user_version`; a later change migrates from it.
const SCHEMA_VERSION: i64 = 1;

/// One recorded channel message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// Channel path below the root, joined with `/` (the root is the empty string).
    pub channel: String,
    pub sender_name: String,
    /// SHA-1 hex of the sender's certificate; `None` for users without one.
    pub sender_cert: Option<String>,
    /// Receive time at the bot (Unix milliseconds).
    pub sent_at: i64,
    /// Message HTML as received.
    pub body: String,
}

/// Count and time span of a channel's messages in a range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    pub count: u64,
    pub oldest: Option<i64>,
    pub newest: Option<i64>,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens or creates the database file.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
        }
        create_private(path)?;
        let conn = Connection::open(path)
            .with_context(|| format!("cannot open history database {}", path.display()))?;
        Self::init(conn)
    }

    /// A store that lives in memory (tests).
    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS message (
                 id INTEGER PRIMARY KEY,
                 channel TEXT NOT NULL,
                 sent_at INTEGER NOT NULL,
                 sender_name TEXT NOT NULL,
                 sender_cert TEXT,
                 body TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS message_channel_time ON message (channel, sent_at);",
        )?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(Self { conn })
    }

    pub fn insert(&self, record: &Record) -> Result<()> {
        self.conn.execute(
            "INSERT INTO message (channel, sent_at, sender_name, sender_cert, body)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                record.channel,
                record.sent_at,
                record.sender_name,
                record.sender_cert,
                record.body
            ],
        )?;
        Ok(())
    }

    /// The newest `limit` messages of `channel` sent in `[from, to)`, oldest first (as
    /// Mumcord's `Store::messages_between`).
    pub fn messages_between(
        &self,
        channel: &str,
        from: i64,
        to: i64,
        limit: u32,
    ) -> Result<Vec<Record>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM (
                 SELECT id, channel, sent_at, sender_name, sender_cert, body FROM message
                 WHERE channel = ?1 AND sent_at >= ?2 AND sent_at < ?3
                 ORDER BY sent_at DESC, id DESC
                 LIMIT ?4)
             ORDER BY sent_at, id",
        )?;
        let rows = stmt.query_map(params![channel, from, to, limit], |row| {
            Ok(Record {
                channel: row.get(1)?,
                sent_at: row.get(2)?,
                sender_name: row.get(3)?,
                sender_cert: row.get(4)?,
                body: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Count and time span of `channel`'s messages sent in `[from, to)`.
    pub fn range_summary(&self, channel: &str, from: i64, to: i64) -> Result<Summary> {
        let (count, oldest, newest): (i64, Option<i64>, Option<i64>) = self.conn.query_row(
            "SELECT COUNT(*), MIN(sent_at), MAX(sent_at) FROM message
             WHERE channel = ?1 AND sent_at >= ?2 AND sent_at < ?3",
            params![channel, from, to],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        Ok(Summary {
            count: count.unsigned_abs(),
            oldest,
            newest,
        })
    }

    /// Deletes messages sent before `before` and, per channel, all but the newest
    /// `max_per_channel`. Returns how many were deleted.
    pub fn prune(&self, before: i64, max_per_channel: u32) -> Result<usize> {
        let old = self
            .conn
            .execute("DELETE FROM message WHERE sent_at < ?1", [before])?;
        let excess = self.conn.execute(
            "DELETE FROM message WHERE id IN (
                 SELECT id FROM (
                     SELECT id, ROW_NUMBER() OVER (
                         PARTITION BY channel ORDER BY sent_at DESC, id DESC) AS newer
                     FROM message)
                 WHERE newer > ?1)",
            [max_per_channel],
        )?;
        Ok(old + excess)
    }

    /// Deletes all but the newest `max` messages of `channel`.
    pub fn trim_channel(&self, channel: &str, max: u32) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM message WHERE channel = ?1 AND id NOT IN (
                 SELECT id FROM message WHERE channel = ?1
                 ORDER BY sent_at DESC, id DESC LIMIT ?2)",
            params![channel, max],
        )?)
    }
}

/// Creates the file owner-only if it does not exist yet; SQLite would create it with the
/// default permissions.
fn create_private(path: &Path) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(path)
        .with_context(|| format!("cannot create history database {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(channel: &str, sent_at: i64, body: &str) -> Record {
        Record {
            channel: channel.to_owned(),
            sender_name: "Alice".to_owned(),
            sender_cert: Some("aa".to_owned()),
            sent_at,
            body: body.to_owned(),
        }
    }

    fn bodies(records: &[Record]) -> Vec<&str> {
        records.iter().map(|r| r.body.as_str()).collect()
    }

    #[test]
    fn ranges_return_the_newest_messages_oldest_first() {
        let store = Store::in_memory().expect("store");
        for (at, body) in [(10, "a"), (20, "b"), (30, "c"), (40, "d")] {
            store.insert(&record("Lobby", at, body)).expect("insert");
        }
        store.insert(&record("Raid", 25, "other")).expect("insert");
        let rows = store.messages_between("Lobby", 10, 40, 2).expect("rows");
        assert_eq!(
            bodies(&rows),
            ["b", "c"],
            "newest 2 in [10, 40), oldest first"
        );
        assert_eq!(rows[0], record("Lobby", 20, "b"));
        assert_eq!(
            store.range_summary("Lobby", 0, 100).expect("summary"),
            Summary {
                count: 4,
                oldest: Some(10),
                newest: Some(40)
            }
        );
        assert_eq!(
            store.range_summary("AFK", 0, 100).expect("summary"),
            Summary {
                count: 0,
                oldest: None,
                newest: None
            }
        );
    }

    #[test]
    fn retention_deletes_by_age_and_by_count_per_channel() {
        let store = Store::in_memory().expect("store");
        for at in 1..=5 {
            store
                .insert(&record("Lobby", at * 10, "x"))
                .expect("insert");
            store.insert(&record("Raid", at * 10, "y")).expect("insert");
        }
        // Age: 10 goes in both channels; count: Lobby and Raid keep their newest 3.
        assert_eq!(store.prune(20, 3).expect("prune"), 2 + 2);
        for channel in ["Lobby", "Raid"] {
            let left: Vec<i64> = store
                .messages_between(channel, 0, 100, 100)
                .expect("rows")
                .iter()
                .map(|r| r.sent_at)
                .collect();
            assert_eq!(left, [30, 40, 50], "{channel}");
        }
        assert_eq!(store.trim_channel("Lobby", 1).expect("trim"), 2);
        assert_eq!(store.range_summary("Lobby", 0, 100).expect("s").count, 1);
        assert_eq!(store.range_summary("Raid", 0, 100).expect("s").count, 3);
    }

    #[cfg(unix)]
    #[test]
    fn the_database_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("sub/history.sqlite3");
        let store = Store::open(&path).expect("open");
        store.insert(&record("Lobby", 1, "hi")).expect("insert");
        drop(store);
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let reopened = Store::open(&path).expect("reopen");
        assert_eq!(reopened.range_summary("Lobby", 0, 10).expect("s").count, 1);
    }
}
