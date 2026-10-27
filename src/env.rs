//! Test-server settings: process environment first, then `testenv/.env` (git-ignored).

use std::path::PathBuf;

/// Default host: the local machine.
pub const HOST: &str = "127.0.0.1";
/// Standard Mumble server port.
pub const DEFAULT_PORT: u16 = 64738;
/// Default host port of the local test server (`testenv/env.sh`).
const TEST_PORT: u16 = 44738;

/// Port of the local test server: `MUMBOT_PORT_BASE` (same variable as `testenv/env.sh`), else
/// 44738.
pub fn test_port() -> u16 {
    std::env::var("MUMBOT_PORT_BASE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(TEST_PORT)
}

/// Reads `key` from the environment, falling back to `testenv/.env`. Empty values count as
/// unset.
pub fn testenv_var(key: &str) -> Option<String> {
    if let Ok(value) = std::env::var(key)
        && !value.is_empty()
    {
        return Some(value);
    }
    let text = std::fs::read_to_string(testenv_dir().join(".env")).ok()?;
    parse_env(&text, key)
}

/// `MUMBOT_TESTENV_DIR`, else the repository's `testenv/`.
fn testenv_dir() -> PathBuf {
    std::env::var_os("MUMBOT_TESTENV_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testenv"))
}

fn parse_env(text: &str, key: &str) -> Option<String> {
    text.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .find(|(k, _)| k.trim() == key)
        .map(|(_, v)| v.trim().trim_matches('"').to_owned())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::parse_env;

    #[test]
    fn parses_env_lines() {
        let text = "# comment\nSUPERUSER_PASSWORD=secret\nSERVER_PASSWORD=\nQUOTED=\"x y\"\n";
        assert_eq!(
            parse_env(text, "SUPERUSER_PASSWORD").as_deref(),
            Some("secret")
        );
        assert_eq!(parse_env(text, "SERVER_PASSWORD"), None);
        assert_eq!(parse_env(text, "QUOTED").as_deref(), Some("x y"));
        assert_eq!(parse_env(text, "MISSING"), None);
    }
}
