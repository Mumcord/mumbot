//! Members of Mumble groups, from the server's answer to an ACL query on one channel.

use std::collections::HashSet;

use anyhow::{Result, bail};
use mumcord_protocol::proto;

/// Registration ids of everyone in any of `groups` on the queried channel: the members inherited
/// from parent channels plus the ones added here, minus the ones removed here. A group the
/// channel does not know is an error, so a typo in the config never silently allows nobody.
pub fn members(acl: &proto::Acl, groups: &[String]) -> Result<HashSet<u32>> {
    let mut members = HashSet::new();
    for name in groups {
        let Some(group) = acl.groups.iter().find(|group| group.name == *name) else {
            bail!(
                "peer: the group {name:?} does not exist on channel {}",
                acl.channel_id
            );
        };
        let removed: HashSet<u32> = group.remove.iter().copied().collect();
        members.extend(
            group
                .inherited_members
                .iter()
                .chain(&group.add)
                .filter(|id| !removed.contains(id)),
        );
    }
    Ok(members)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(name: &str, add: &[u32], remove: &[u32], inherited: &[u32]) -> proto::acl::ChanGroup {
        proto::acl::ChanGroup {
            name: name.to_owned(),
            add: add.to_vec(),
            remove: remove.to_vec(),
            inherited_members: inherited.to_vec(),
            ..Default::default()
        }
    }

    #[test]
    fn members_are_inherited_plus_added_minus_removed() {
        let acl = proto::Acl {
            channel_id: 4,
            groups: vec![
                group("relay-users", &[3, 4], &[2], &[1, 2]),
                group("admin", &[9], &[], &[]),
                group("friends", &[5], &[], &[]),
            ],
            ..Default::default()
        };
        let both = members(&acl, &["relay-users".to_owned(), "friends".to_owned()]).expect("ok");
        assert_eq!(both, HashSet::from([1, 3, 4, 5]));
        let error = members(&acl, &["relay-user".to_owned()]).expect_err("typo");
        assert!(error.to_string().contains("\"relay-user\""), "{error}");
    }
}
