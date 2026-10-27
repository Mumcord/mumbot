//! Persistent bot certificates: generated once per name and reused, so a bot keeps its identity
//! (cert hash) across renames and reconnects. Stored outside the repository.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mumcord_protocol::ClientIdentity;
use mumcord_protocol::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

const CERT_FILE: &str = "cert.der";
const KEY_FILE: &str = "key.der";

/// `MUMBOT_DATA`, else `$XDG_DATA_HOME/mumbot`, else `~/.local/share/mumbot`.
pub fn default_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("MUMBOT_DATA") {
        return PathBuf::from(dir);
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(std::env::temp_dir);
    data.join("mumbot")
}

/// Loads the certificate `name` from `dir/certs/<name>/`, generating it on first use.
pub fn load_or_create(dir: &Path, name: &str) -> Result<ClientIdentity> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("certificate name {name:?} may only contain letters, digits, '-' and '_'");
    }
    let dir = dir.join("certs").join(name);
    let (cert_path, key_path) = (dir.join(CERT_FILE), dir.join(KEY_FILE));
    if !cert_path.exists() {
        let generated = rcgen::generate_simple_self_signed(vec![format!("mumbot-{name}")])
            .context("generating bot certificate")?;
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        write_private(&key_path, &generated.signing_key.serialize_der())?;
        // Written last: its presence marks a complete pair.
        fs::write(&cert_path, generated.cert.der())
            .with_context(|| format!("writing {}", cert_path.display()))?;
    }
    let cert = fs::read(&cert_path).with_context(|| format!("reading {}", cert_path.display()))?;
    let key = fs::read(&key_path).with_context(|| format!("reading {}", key_path.display()))?;
    Ok(ClientIdentity {
        chain: vec![CertificateDer::from(cert)],
        key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
    })
}

fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(
        &mut options
            .open(path)
            .with_context(|| format!("writing {}", path.display()))?,
        data,
    )
    .with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificate_is_created_once_and_reused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = load_or_create(dir.path(), "alice").expect("create");
        let second = load_or_create(dir.path(), "alice").expect("load");
        assert_eq!(first.chain, second.chain);
        let other = load_or_create(dir.path(), "bob").expect("create");
        assert_ne!(first.chain, other.chain);
    }

    #[test]
    fn rejects_path_like_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(load_or_create(dir.path(), "../x").is_err());
        assert!(load_or_create(dir.path(), "").is_err());
    }
}
