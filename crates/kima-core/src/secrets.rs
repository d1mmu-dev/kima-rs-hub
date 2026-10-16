//! The two server secrets, generated on first start and kept in the data dir.
//!
//! Values supplied through the environment (`SESSION_SECRET`,
//! `SETTINGS_ENCRYPTION_KEY`) are never written to the file. If they are later
//! removed, the server falls back to the stored generated key, and settings
//! encrypted under the env key become unreadable.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rand::RngCore;
use rand::rngs::OsRng;
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
    let stored = match read_stored(&path)? {
        Some(stored) => stored,
        None => {
            let fresh = SecretsFile {
                session_secret: random_secret(),
                settings_encryption_key: random_secret(),
            };
            let json = serde_json::to_string_pretty(&fresh).expect("secrets serialise");
            if publish_new(&path, &json)? {
                fresh
            } else {
                // Another process won the first-start race and published its file first.
                read_stored(&path)?.ok_or_else(|| SecretsError::Io {
                    path: path.clone(),
                    source: io::Error::new(
                        io::ErrorKind::NotFound,
                        "secrets file vanished during first start",
                    ),
                })?
            }
        }
    };
    Ok(Secrets {
        session_secret: env_session.unwrap_or(stored.session_secret),
        settings_encryption_key: env_key.unwrap_or(stored.settings_encryption_key),
    })
}

fn random_secret() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    STANDARD.encode(bytes)
}

/// Reads the stored secrets; `None` when the file does not exist yet.
fn read_stored(path: &Path) -> Result<Option<SecretsFile>, SecretsError> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str::<SecretsFile>(&text)
            .map(Some)
            .map_err(|source| SecretsError::Corrupt {
                path: path.to_path_buf(),
                source,
            }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(SecretsError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Publishes `contents` at `path` only if nothing is there yet. Returns `Ok(false)`
/// without touching the file when it already exists, so a racing first start cannot
/// replace secrets another process has already handed out.
///
/// Writes to a uniquely named temporary file (created exclusively, mode 0600 on unix)
/// first, so a crash never leaves a half-written secrets file and a stale or planted
/// temporary file is never reused.
fn publish_new(path: &Path, contents: &str) -> Result<bool, SecretsError> {
    let io_err = |source: io::Error| SecretsError::Io {
        path: path.to_path_buf(),
        source,
    };
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(io_err)?;
    }
    let tmp = path.with_extension(format!("json.{:016x}.tmp", OsRng.next_u64()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp).map_err(io_err)?;
    let written = file
        .write_all(contents.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(source) = written {
        let _ = fs::remove_file(&tmp);
        return Err(io_err(source));
    }
    // hard_link refuses to replace an existing path, unlike rename.
    let linked = fs::hard_link(&tmp, path);
    let _ = fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(source) => Err(io_err(source)),
    }
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
    fn publish_onto_existing_file_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SECRETS_FILE);
        fs::write(
            &path,
            r#"{"session_secret":"a","settings_encryption_key":"b"}"#,
        )
        .unwrap();
        let published = publish_new(
            &path,
            r#"{"session_secret":"x","settings_encryption_key":"y"}"#,
        )
        .unwrap();
        assert!(!published, "existing file must not be replaced");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            r#"{"session_secret":"a","settings_encryption_key":"b"}"#
        );
        let leftovers = fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(leftovers, 1, "temporary file is cleaned up");
    }

    #[test]
    fn publish_creates_file_with_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SECRETS_FILE);
        assert!(publish_new(&path, "{}").unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{}");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
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
