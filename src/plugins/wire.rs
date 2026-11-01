//! The plugin suggestion message of the plugins module (data id `mumcord.srvplugins`, Mumcord
//! spec 06, FR-6.77) and the hello feature it answers.
//!
//! The message is compact JSON `{"v":1,"seq":…,"plugins":[{"id","min","src","key"?,"for"}]}` of
//! at most 1000 bytes (the server's plugin data limit). There is no `level`: every suggestion is
//! "recommended" (FR-6.78). Clients send nothing back.

use serde::Serialize;

/// Plugin data id of the suggestion message.
pub const DATA_ID: &str = "mumcord.srvplugins";
/// The hello feature of Mumcord clients that take plugin suggestions.
pub const FEATURE: &str = "server-plugins";
/// The version this bot sends.
pub const VERSION: u32 = 1;

/// One suggested plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Suggestion {
    pub id: String,
    /// Lowest version the community uses (semver).
    pub min: String,
    /// `official`, or the HTTPS address of a third-party repository index.
    pub src: String,
    /// The third-party repository's minisign public key; never for `official`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// What the community uses it for (≤ 40 characters).
    #[serde(rename = "for")]
    pub purpose: String,
}

#[derive(Serialize)]
struct Message<'a> {
    v: u32,
    seq: u32,
    plugins: &'a [Suggestion],
}

/// The message for one announcement.
pub fn encode(seq: u32, plugins: &[Suggestion]) -> Vec<u8> {
    // Plain structs of strings and numbers always serialize.
    serde_json::to_vec(&Message {
        v: VERSION,
        seq,
        plugins,
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_matches_the_spec() {
        let plugins = [
            Suggestion {
                id: "dice".into(),
                min: "1.0.0".into(),
                src: "official".into(),
                key: None,
                purpose: "board game nights".into(),
            },
            Suggestion {
                id: "guild-calendar".into(),
                min: "0.9.0".into(),
                src: "https://plugins.example.org/v1/index.json".into(),
                key: Some("RWQ".into()),
                purpose: "raid sign-ups".into(),
            },
        ];
        let json: serde_json::Value = serde_json::from_slice(&encode(3, &plugins)).expect("json");
        assert_eq!(
            json,
            serde_json::json!({"v":1,"seq":3,"plugins":[
                {"id":"dice","min":"1.0.0","src":"official","for":"board game nights"},
                {"id":"guild-calendar","min":"0.9.0",
                    "src":"https://plugins.example.org/v1/index.json","key":"RWQ",
                    "for":"raid sign-ups"}
            ]})
        );
    }
}
