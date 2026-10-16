//! The two server secrets, generated on first start and kept in the data dir.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rand::RngCore;
use serde::{Deserialize, Serialize};

pub const SECRETS_FILE: &str = "secrets.json";

#[derive(Clone, PartialEq)]
pub struct Secrets {
    pub session_secret: String,
    pub settings_encryption_key: String,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secrets { .. }")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    #[error("cannot read or write {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "{path} is not valid secrets JSON ({source}); refusing to replace it, since settings \
         encrypted with the old key would become unreadable"
    )]
    Corrupt {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

#[derive(Serialize, Deserialize)]
struct SecretsFile {
    session_secret: String,
    settings_encryption_key: String,
}

pub fn load_or_generate(
    data_dir: &Path,
    env_session: Option<String>,
    env_key: Option<String>,
) -> Result<Secrets, SecretsError> {
    let path = data_dir.join(SECRETS_FILE);
    let stored = match fs::read_to_string(&path) {
        Ok(text) => {
            serde_json::from_str::<SecretsFile>(&text).map_err(|source| SecretsError::Corrupt {
                path: path.clone(),
                source,
            })?
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let fresh = SecretsFile {
                session_secret: random_secret(),
                settings_encryption_key: random_secret(),
            };
            let json = serde_json::to_string_pretty(&fresh).expect("secrets serialise");
            write_private(&path, &json)?;
            fresh
        }
        Err(source) => return Err(SecretsError::Io { path, source }),
    };
    Ok(Secrets {
        session_secret: env_session.unwrap_or(stored.session_secret),
        settings_encryption_key: env_key.unwrap_or(stored.settings_encryption_key),
    })
}

fn random_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    STANDARD.encode(bytes)
}

/// Writes through a temporary file so a crash never leaves a half-written secrets file.
fn write_private(path: &Path, contents: &str) -> Result<(), SecretsError> {
    let io_err = |source: io::Error| SecretsError::Io {
        path: path.to_path_buf(),
        source,
    };
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(io_err)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp).map_err(io_err)?;
    file.write_all(contents.as_bytes()).map_err(io_err)?;
    file.sync_all().map_err(io_err)?;
    fs::rename(&tmp, path).map_err(io_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_start_generates_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_generate(dir.path(), None, None).unwrap();
        assert!(first.session_secret.len() >= 40, "32 random bytes, base64");
        assert_ne!(first.session_secret, first.settings_encryption_key);

        let second = load_or_generate(dir.path(), None, None).unwrap();
        assert_eq!(first, second, "same secrets on the next start");
    }

    #[test]
    fn creates_missing_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b");
        load_or_generate(&nested, None, None).unwrap();
        assert!(nested.join(SECRETS_FILE).exists());
    }

    #[cfg(unix)]
    #[test]
    fn file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        load_or_generate(dir.path(), None, None).unwrap();
        let mode = fs::metadata(dir.path().join(SECRETS_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn env_values_win() {
        let dir = tempfile::tempdir().unwrap();
        let s =
            load_or_generate(dir.path(), Some("from-env".into()), Some("key-env".into())).unwrap();
        assert_eq!(s.session_secret, "from-env");
        assert_eq!(s.settings_encryption_key, "key-env");
    }

    #[test]
    fn corrupt_file_is_an_error_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SECRETS_FILE);
        fs::write(&path, "{ not json").unwrap();
        let err = load_or_generate(dir.path(), None, None).unwrap_err();
        assert!(matches!(err, SecretsError::Corrupt { .. }));
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json");
    }
}
