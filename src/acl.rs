//! Superuser edits on the local test servers: ACLs (regular protocol `ACL` message) and channel
//! descriptions (`ChannelState`).

use anyhow::{Result, bail};
use mumcord_protocol::{permissions, proto};

use crate::Bot;

impl Bot {
    /// Denies "Listen" for everyone on `channel` and its subchannels (mumcord AC-1.16). Replaces the
    /// channel's own ACL; the test channels have none. Returns once the server applied it
    /// (needs a 1.4+ server).
    pub async fn deny_listen(&self, channel: &str) -> Result<()> {
        self.deny_for_everyone(channel, permissions::LISTEN).await
    }

    /// Removes the channel's own ACL entries again (back to inheriting from the parent).
    pub async fn allow_listen(&self, channel: &str) -> Result<()> {
        self.set_acl(channel, Vec::new()).await
    }

    /// Denies "Speak" for everyone on `channel` and its subchannels: the server then suppresses
    /// everyone in it except the superuser. Replaces the channel's own ACL.
    pub async fn deny_speak(&self, channel: &str) -> Result<()> {
        self.deny_for_everyone(channel, permissions::SPEAK).await
    }

    /// Removes the channel's own ACL entries again (back to inheriting from the parent).
    pub async fn allow_speak(&self, channel: &str) -> Result<()> {
        self.set_acl(channel, Vec::new()).await
    }

    /// Denies "Text message" for everyone on `channel` and its subchannels (mumcord FR-2.47). Replaces
    /// the channel's own ACL.
    pub async fn deny_text(&self, channel: &str) -> Result<()> {
        self.deny_for_everyone(channel, permissions::TEXT_MESSAGE)
            .await
    }

    /// Removes the channel's own ACL entries again (back to inheriting from the parent).
    pub async fn allow_text(&self, channel: &str) -> Result<()> {
        self.set_acl(channel, Vec::new()).await
    }

    /// Lets only holders of the access token `token` enter `channel` (a channel password,
    /// mumcord FR-2.78): denies "Enter" for everyone, then grants it to the token group `#<token>`.
    /// Replaces the channel's own ACL.
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

    async fn deny_for_everyone(&self, channel: &str, permission: u32) -> Result<()> {
        let deny = proto::acl::ChanAcl {
            apply_here: Some(true),
            apply_subs: Some(true),
            // The server ignores entries marked as inherited.
            inherited: Some(false),
            user_id: None,
            group: Some("all".to_owned()),
            grant: Some(0),
            deny: Some(permission),
        };
        self.set_acl(channel, vec![deny]).await
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

    /// Sets the description of `channel`; an empty `html` removes it (mumcord FR-1.48b). Returns once
    /// the server sent the changed channel state back.
    pub async fn set_description(&self, channel: &str, html: &str) -> Result<()> {
        if !self.config().superuser {
            bail!("changing descriptions needs the superuser bot");
        }
        let channel_id = self.channel_id(channel)?;
        let state = proto::ChannelState {
            channel_id: Some(channel_id),
            description: Some(html.to_owned()),
            ..Default::default()
        };
        self.send_and_await_channel_update(
            channel_id,
            "the server to apply the description",
            || Ok(self.handle().send_channel_state(state)?),
        )
        .await
    }
}
