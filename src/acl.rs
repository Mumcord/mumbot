//! Superuser ACL edits on the local test server (regular protocol `ACL` message), used by the
//! live tests to lock a channel behind a password.

use anyhow::{Result, bail};
use mumcord_protocol::{permissions, proto};

use crate::Bot;

impl Bot {
    /// Lets only holders of the access token `token` enter `channel` (a channel password, the
    /// mechanism of mumcord FR-2.78): denies "Enter" for everyone, then grants it to the token
    /// group `#<token>`. Replaces the channel's own ACL.
    pub async fn require_token(&self, channel: &str, token: &str) -> Result<()> {
        let entry = |group: String, grant: u32, deny: u32| proto::acl::ChanAcl {
            apply_here: Some(true),
            apply_subs: Some(true),
            inherited: Some(false),
            user_id: None,
            group: Some(group),
            grant: Some(grant),
            deny: Some(deny),
        };
        // Later entries win, so the token grant overrides the deny for everyone.
        let acls = vec![
            entry("all".to_owned(), 0, permissions::ENTER),
            entry(format!("#{token}"), permissions::ENTER, 0),
        ];
        self.set_acl(channel, acls).await
    }

    async fn set_acl(&self, channel: &str, acls: Vec<proto::acl::ChanAcl>) -> Result<()> {
        if !self.config().superuser {
            bail!("changing ACLs needs the superuser bot");
        }
        let channel_id = self.channel_id(channel)?;
        let acl = proto::Acl {
            channel_id,
            inherit_acls: Some(true),
            groups: Vec::new(),
            acls,
            query: Some(false),
        };
        // An ACL write has no reply, but 1.4+ servers then resend the channel's state (enter
        // restrictions) to everyone, after the new ACL is in effect.
        self.send_and_await_channel_update(channel_id, "the server to apply the ACL", || {
            Ok(self.handle().send_acl(acl)?)
        })
        .await
    }
}
