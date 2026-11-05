use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Clone, PartialEq)]
pub struct Config {
    pub database_url: String,
    pub data_dir: PathBuf,
    pub music_path: PathBuf,
    pub bind_addr: SocketAddr,
    pub frontend_dir: Option<PathBuf>,
    /// `KIMA_WEB_BUILD_DIR`: the `vertigo build` output of `kima-web`. Set means the embedded SSR
    /// server runs and `kima-web`'s routes are proxied to it; unset (the default) means neither.
    pub web_build_dir: Option<PathBuf>,
    /// `KIMA_WEB_SSR_PORT`: the loopback port of the embedded SSR server.
    pub web_ssr_port: u16,
    pub allowed_origins: Vec<String>,
    pub log_level: String,
    pub session_secret: Option<String>,
    pub settings_encryption_key: Option<String>,
    pub watch_mode: WatchMode,
    pub watch_poll_secs: u64,
    pub organize_target: Option<PathBuf>,
    pub transcode_cache_mb: u64,
    /// `KIMA_ARTWORK_CACHE_MB`: size cap of the cover-art cache.
    pub artwork_cache_mb: u64,
    /// `KIMA_PUBLIC_URL`: where clients reach this server, without a trailing slash.
    pub public_url: Option<String>,
    /// `KIMA_LIBRARY_SYNC_CRON`: 5-field cron (UTC) of the safety-net reconcile; `None` when `off`.
    /// The expression is validated where it is parsed (startup in `kima-server`).
    pub library_sync_cron: Option<String>,
    /// `KIMA_IDENTIFY=off`: no outbound lookups, whatever the admin setting says (D-111).
    pub identify_env_off: bool,
    /// `KIMA_CONTACT`: the contact in the outbound User-Agent (spec §7.1).
    pub contact: String,
    /// `MUSICBRAINZ_BASE_URL`: MusicBrainz or a mirror, without a trailing slash.
    pub musicbrainz_base_url: String,
    /// `KIMA_FINGERPRINT_WORKERS`: fingerprint pool size; half the cores, at least 1 (spec §9.3).
    pub fingerprint_workers: usize,
    /// `KIMA_CLAP_MODEL_DIR`: directory containing the pinned CLAP audio model.
    pub clap_model_dir: Option<PathBuf>,
    /// `DISABLE_CLAP=true` skips CLAP initialization.
    pub disable_clap: bool,
    /// `KIMA_MUSICNN_MODEL_DIR`: directory containing the pinned MusiCNN analysis models
    /// (core + 10 heads).
    pub musicnn_model_dir: Option<PathBuf>,
    /// `DISABLE_MUSICNN=true` skips MusiCNN initialization (DSP-only analysis).
    pub disable_musicnn: bool,
    /// `KIMA_SNAPSHOT_DIR`: absolute, private (0700, server-owned) directory for unnamed input
    /// snapshots. It must be on the SAME mount as the library, which therefore must be mounted
    /// read-write for the server. Unset: embeddings fail closed with `SNAPSHOT_UNAVAILABLE`.
    pub snapshot_dir: Option<PathBuf>,
    /// `KIMA_SNAPSHOT_MAX_BYTES`: largest input snapshotted; `None` is the `kima-ml` default.
    pub snapshot_max_bytes: Option<u64>,
    /// `KIMA_SNAPSHOT_MAX_CONCURRENT`: live snapshots at once; `None` is the `kima-ml` default.
    pub snapshot_max_concurrent: Option<usize>,
    /// `KIMA_VIBE_ASSET_DIR`: directory with the private vibe assets (feel probe, mood heads, SHA-256 pinned).
    /// Unset: the vibe capabilities are `unavailable`; the server still boots.
    pub vibe_asset_dir: Option<PathBuf>,
    /// `KIMA_VIBE_EXACT_MAX`: text search scans exactly up to this many eligible tracks, then uses the HNSW
    /// index; 0 always uses the index. Default [`DEFAULT_VIBE_EXACT_MAX`].
    pub vibe_exact_max: u64,
    /// `ACOUSTID_DISABLED=true` (any case): no AcoustID rung, `fpcalc: false` (upstream's name).
    pub acoustid_disabled: bool,
    /// `ACOUSTID_CLIENT_KEY`: the AcoustID client key after the stored setting (spec §8.4). A
    /// secret: never printed.
    pub acoustid_client_key: Option<String>,
    /// `FANART_API_KEY`: the fanart.tv key when no enabled key is stored (spec §15.1). A secret:
    /// never printed.
    pub fanart_api_key: Option<String>,
}

// Never prints a secret.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hidden = |v: &Option<String>| v.as_ref().map(|_| "..");
        f.debug_struct("Config")
            .field("database_url", &self.database_url)
            .field("data_dir", &self.data_dir)
            .field("music_path", &self.music_path)
            .field("bind_addr", &self.bind_addr)
            .field("frontend_dir", &self.frontend_dir)
            .field("web_build_dir", &self.web_build_dir)
            .field("web_ssr_port", &self.web_ssr_port)
            .field("allowed_origins", &self.allowed_origins)
            .field("log_level", &self.log_level)
            .field("session_secret", &hidden(&self.session_secret))
            .field(
                "settings_encryption_key",
                &hidden(&self.settings_encryption_key),
            )
            .field("watch_mode", &self.watch_mode)
            .field("watch_poll_secs", &self.watch_poll_secs)
            .field("organize_target", &self.organize_target)
            .field("transcode_cache_mb", &self.transcode_cache_mb)
            .field("artwork_cache_mb", &self.artwork_cache_mb)
            .field("public_url", &self.public_url)
            .field("library_sync_cron", &self.library_sync_cron)
            .field("identify_env_off", &self.identify_env_off)
            .field("contact", &self.contact)
            .field("musicbrainz_base_url", &self.musicbrainz_base_url)
            .field("fingerprint_workers", &self.fingerprint_workers)
            .field("clap_model_dir", &self.clap_model_dir)
            .field("disable_clap", &self.disable_clap)
            .field("musicnn_model_dir", &self.musicnn_model_dir)
            .field("disable_musicnn", &self.disable_musicnn)
            .field("snapshot_dir", &self.snapshot_dir)
            .field("snapshot_max_bytes", &self.snapshot_max_bytes)
            .field("snapshot_max_concurrent", &self.snapshot_max_concurrent)
            .field("vibe_asset_dir", &self.vibe_asset_dir)
            .field("vibe_exact_max", &self.vibe_exact_max)
            .field("acoustid_disabled", &self.acoustid_disabled)
            .field("acoustid_client_key", &hidden(&self.acoustid_client_key))
            .field("fanart_api_key", &hidden(&self.fanart_api_key))
            .finish()
    }
}

/// `KIMA_VIBE_EXACT_MAX` when unset (design 11.3, chosen from a 0.17-0.4 s exact scan at 100k tracks).
pub const DEFAULT_VIBE_EXACT_MAX: u64 = 200_000;

/// `KIMA_CONTACT` when unset.
/// Loopback port of the embedded `kima-web` SSR server when `KIMA_WEB_SSR_PORT` is unset.
pub const DEFAULT_WEB_SSR_PORT: u16 = 3031;

pub const DEFAULT_CONTACT: &str = "https://github.com/d1mmu-dev/kima-rs-hub";
/// `MUSICBRAINZ_BASE_URL` when unset.
pub const DEFAULT_MUSICBRAINZ_BASE_URL: &str = "https://musicbrainz.org/ws/2";

/// `raw` without trailing slashes, when it is an `http://` or `https://` URL with a host.
fn http_base(raw: &str) -> Option<String> {
    let trimmed = raw.trim_end_matches('/');
    let rest = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))?;
    let host = rest.split('/').next().unwrap_or("");
    (!host.is_empty()).then(|| trimmed.to_string())
}

/// How the library folder is watched for changes (`KIMA_WATCH_MODE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchMode {
    /// Filesystem events, falling back to polling where they are unavailable.
    Auto,
    /// Periodic scans only.
    Poll,
    /// No watching.
    Off,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ConfigError {
    #[error("missing required environment variable {0}")]
    Missing(&'static str),
    #[error("invalid value for {name}: {value:?}")]
    Invalid { name: &'static str, value: String },
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let value = |key: &str| {
            get(key)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };

        let database_url = value("DATABASE_URL").ok_or(ConfigError::Missing("DATABASE_URL"))?;

        let port_raw = value("PORT").unwrap_or_else(|| "3030".into());
        let port: u16 = port_raw.parse().map_err(|_| ConfigError::Invalid {
            name: "PORT",
            value: port_raw.clone(),
        })?;
        let host_raw = value("KIMA_BIND_HOST").unwrap_or_else(|| "0.0.0.0".into());
        let host: std::net::IpAddr = host_raw.parse().map_err(|_| ConfigError::Invalid {
            name: "KIMA_BIND_HOST",
            value: host_raw.clone(),
        })?;

        let allowed_origins = value("ALLOWED_ORIGINS")
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();

        let watch_mode = match value("KIMA_WATCH_MODE") {
            None => WatchMode::Auto,
            Some(raw) => match raw.to_ascii_lowercase().as_str() {
                "auto" => WatchMode::Auto,
                "poll" => WatchMode::Poll,
                "off" => WatchMode::Off,
                _ => {
                    return Err(ConfigError::Invalid {
                        name: "KIMA_WATCH_MODE",
                        value: raw,
                    });
                }
            },
        };
        let watch_poll_secs = match value("KIMA_WATCH_POLL_SECS") {
            None => 60,
            Some(raw) => match raw.parse::<u64>() {
                Ok(n) if n > 0 => n,
                _ => {
                    return Err(ConfigError::Invalid {
                        name: "KIMA_WATCH_POLL_SECS",
                        value: raw,
                    });
                }
            },
        };
        let transcode_cache_mb = match value("KIMA_TRANSCODE_CACHE_MB") {
            None => 2048,
            Some(raw) => raw.parse::<u64>().map_err(|_| ConfigError::Invalid {
                name: "KIMA_TRANSCODE_CACHE_MB",
                value: raw,
            })?,
        };

        let artwork_cache_mb = match value("KIMA_ARTWORK_CACHE_MB") {
            None => 512,
            Some(raw) => raw.parse::<u64>().map_err(|_| ConfigError::Invalid {
                name: "KIMA_ARTWORK_CACHE_MB",
                value: raw,
            })?,
        };

        let musicbrainz_base_url = match value("MUSICBRAINZ_BASE_URL") {
            None => DEFAULT_MUSICBRAINZ_BASE_URL.to_string(),
            Some(raw) => http_base(&raw).ok_or(ConfigError::Invalid {
                name: "MUSICBRAINZ_BASE_URL",
                value: raw,
            })?,
        };

        let public_url = match value("KIMA_PUBLIC_URL") {
            None => None,
            Some(raw) => {
                let trimmed = raw.trim_end_matches('/');
                let host = trimmed
                    .strip_prefix("https://")
                    .or_else(|| trimmed.strip_prefix("http://"));
                match host {
                    Some(host) if !host.is_empty() => Some(trimmed.to_string()),
                    _ => {
                        return Err(ConfigError::Invalid {
                            name: "KIMA_PUBLIC_URL",
                            value: raw,
                        });
                    }
                }
            }
        };

        let library_sync_cron = match value("KIMA_LIBRARY_SYNC_CRON") {
            None => Some("0 4 * * *".to_string()),
            Some(raw) if raw.eq_ignore_ascii_case("off") => None,
            Some(raw) => Some(raw),
        };

        let identify_env_off = match value("KIMA_IDENTIFY") {
            None => false,
            Some(raw) => match raw.to_ascii_lowercase().as_str() {
                "on" => false,
                "off" => true,
                _ => {
                    return Err(ConfigError::Invalid {
                        name: "KIMA_IDENTIFY",
                        value: raw,
                    });
                }
            },
        };

        let web_ssr_port = match value("KIMA_WEB_SSR_PORT") {
            None => DEFAULT_WEB_SSR_PORT,
            Some(raw) => match raw.parse::<u16>() {
                Ok(n) if n > 0 => n,
                _ => {
                    return Err(ConfigError::Invalid {
                        name: "KIMA_WEB_SSR_PORT",
                        value: raw,
                    });
                }
            },
        };

        let fingerprint_workers = match value("KIMA_FINGERPRINT_WORKERS") {
            None => std::thread::available_parallelism()
                .map_or(1, |n| n.get() / 2)
                .max(1),
            Some(raw) => match raw.parse::<usize>() {
                Ok(n) if n > 0 => n,
                _ => {
                    return Err(ConfigError::Invalid {
                        name: "KIMA_FINGERPRINT_WORKERS",
                        value: raw,
                    });
                }
            },
        };
        let positive = |name: &'static str| -> Result<Option<u64>, ConfigError> {
            match value(name) {
                None => Ok(None),
                Some(raw) => match raw.parse::<u64>() {
                    Ok(n) if n > 0 => Ok(Some(n)),
                    _ => Err(ConfigError::Invalid { name, value: raw }),
                },
            }
        };
        let snapshot_max_bytes = positive("KIMA_SNAPSHOT_MAX_BYTES")?;
        let snapshot_max_concurrent = positive("KIMA_SNAPSHOT_MAX_CONCURRENT")?
            .map(usize::try_from)
            .transpose()
            .map_err(|_| ConfigError::Invalid {
                name: "KIMA_SNAPSHOT_MAX_CONCURRENT",
                value: "too large".into(),
            })?;
        let vibe_exact_max = match value("KIMA_VIBE_EXACT_MAX") {
            None => DEFAULT_VIBE_EXACT_MAX,
            Some(raw) => raw.parse::<u64>().map_err(|_| ConfigError::Invalid {
                name: "KIMA_VIBE_EXACT_MAX",
                value: raw,
            })?,
        };
        // Upstream compares with `'true'`; any other value leaves AcoustID on.
        let acoustid_disabled =
            value("ACOUSTID_DISABLED").is_some_and(|v| v.eq_ignore_ascii_case("true"));

        Ok(Self {
            database_url,
            data_dir: value("KIMA_DATA_DIR")
                .unwrap_or_else(|| "/data".into())
                .into(),
            music_path: value("MUSIC_PATH")
                .unwrap_or_else(|| "/music".into())
                .into(),
            bind_addr: SocketAddr::new(host, port),
            frontend_dir: value("KIMA_FRONTEND_DIR").map(PathBuf::from),
            web_build_dir: value("KIMA_WEB_BUILD_DIR").map(PathBuf::from),
            web_ssr_port,
            allowed_origins,
            log_level: value("LOG_LEVEL").unwrap_or_else(|| "info".into()),
            session_secret: value("SESSION_SECRET"),
            settings_encryption_key: value("SETTINGS_ENCRYPTION_KEY"),
            watch_mode,
            watch_poll_secs,
            organize_target: value("KIMA_ORGANIZE_TARGET").map(PathBuf::from),
            transcode_cache_mb,
            artwork_cache_mb,
            public_url,
            library_sync_cron,
            identify_env_off,
            contact: value("KIMA_CONTACT").unwrap_or_else(|| DEFAULT_CONTACT.into()),
            musicbrainz_base_url,
            fingerprint_workers,
            clap_model_dir: value("KIMA_CLAP_MODEL_DIR").map(PathBuf::from),
            disable_clap: value("DISABLE_CLAP").is_some_and(|v| v.eq_ignore_ascii_case("true")),
            musicnn_model_dir: value("KIMA_MUSICNN_MODEL_DIR").map(PathBuf::from),
            disable_musicnn: value("DISABLE_MUSICNN")
                .is_some_and(|v| v.eq_ignore_ascii_case("true")),
            snapshot_dir: value("KIMA_SNAPSHOT_DIR").map(PathBuf::from),
            snapshot_max_bytes,
            snapshot_max_concurrent,
            vibe_asset_dir: value("KIMA_VIBE_ASSET_DIR").map(PathBuf::from),
            vibe_exact_max,
            acoustid_disabled,
            acoustid_client_key: value("ACOUSTID_CLIENT_KEY"),
            fanart_api_key: value("FANART_API_KEY"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    #[test]
    fn database_url_is_required() {
        assert_eq!(
            Config::from_lookup(lookup(&[])),
            Err(ConfigError::Missing("DATABASE_URL"))
        );
        assert_eq!(
            Config::from_lookup(lookup(&[("DATABASE_URL", "  ")])),
            Err(ConfigError::Missing("DATABASE_URL"))
        );
    }

    #[test]
    fn disable_clap_is_case_insensitive_true_only() {
        for (value, expected) in [
            ("true", true),
            ("TRUE", true),
            ("false", false),
            ("1", false),
        ] {
            let config = Config::from_lookup(lookup(&[
                ("DATABASE_URL", "postgres://x"),
                ("DISABLE_CLAP", value),
            ]))
            .unwrap();
            assert_eq!(config.disable_clap, expected);
        }
    }

    #[test]
    fn vibe_settings_parse_and_default_to_unset() {
        let base = [("DATABASE_URL", "postgres://x")];
        let config = Config::from_lookup(lookup(&base)).unwrap();
        assert_eq!(
            config.vibe_asset_dir, None,
            "the server boots without vibe assets"
        );
        assert_eq!(config.vibe_exact_max, DEFAULT_VIBE_EXACT_MAX);
        assert_eq!(DEFAULT_VIBE_EXACT_MAX, 200_000);
        let config = Config::from_lookup(lookup(&[
            base[0],
            ("KIMA_VIBE_ASSET_DIR", "/models/vibe"),
            ("KIMA_VIBE_EXACT_MAX", " 0 "),
        ]))
        .unwrap();
        assert_eq!(config.vibe_asset_dir, Some(PathBuf::from("/models/vibe")));
        assert_eq!(config.vibe_exact_max, 0, "0 forces the index path");
        for bad in ["-1", "lots", "1.5"] {
            assert_eq!(
                Config::from_lookup(lookup(&[base[0], ("KIMA_VIBE_EXACT_MAX", bad)])),
                Err(ConfigError::Invalid {
                    name: "KIMA_VIBE_EXACT_MAX",
                    value: bad.into()
                })
            );
        }
    }

    #[test]
    fn snapshot_settings_parse_and_default_to_unset() {
        let base = [("DATABASE_URL", "postgres://x")];
        let config = Config::from_lookup(lookup(&base)).unwrap();
        assert_eq!(config.snapshot_dir, None);
        assert_eq!(config.snapshot_max_bytes, None);
        assert_eq!(config.snapshot_max_concurrent, None);
        let config = Config::from_lookup(lookup(&[
            base[0],
            ("KIMA_SNAPSHOT_DIR", "/music/.kima-snapshots"),
            ("KIMA_SNAPSHOT_MAX_BYTES", " 1048576 "),
            ("KIMA_SNAPSHOT_MAX_CONCURRENT", "3"),
        ]))
        .unwrap();
        assert_eq!(
            config.snapshot_dir,
            Some(PathBuf::from("/music/.kima-snapshots"))
        );
        assert_eq!(config.snapshot_max_bytes, Some(1_048_576));
        assert_eq!(config.snapshot_max_concurrent, Some(3));
        for name in ["KIMA_SNAPSHOT_MAX_BYTES", "KIMA_SNAPSHOT_MAX_CONCURRENT"] {
            for bad in ["0", "-1", "lots", "1.5"] {
                assert_eq!(
                    Config::from_lookup(lookup(&[base[0], (name, bad)])),
                    Err(ConfigError::Invalid {
                        name,
                        value: bad.into()
                    })
                );
            }
        }
    }

    #[test]
    fn clap_model_directory_is_configurable() {
        let config = Config::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://x"),
            ("KIMA_CLAP_MODEL_DIR", "/models/clap"),
        ]))
        .unwrap();
        assert!(format!("{config:?}").contains("/models/clap"));
    }

    #[test]
    fn musicnn_model_directory_is_configurable() {
        let config = Config::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://x"),
            ("KIMA_MUSICNN_MODEL_DIR", "/models/musicnn"),
        ]))
        .unwrap();
        assert_eq!(
            config.musicnn_model_dir,
            Some(PathBuf::from("/models/musicnn"))
        );
        assert!(format!("{config:?}").contains("/models/musicnn"));
    }

    #[test]
    fn disable_musicnn_is_case_insensitive_true_only() {
        for (value, expected) in [
            ("true", true),
            ("TRUE", true),
            ("false", false),
            ("1", false),
        ] {
            let config = Config::from_lookup(lookup(&[
                ("DATABASE_URL", "postgres://x"),
                ("DISABLE_MUSICNN", value),
            ]))
            .unwrap();
            assert_eq!(config.disable_musicnn, expected);
        }
    }

    #[test]
    fn musicnn_settings_default_to_unset() {
        let config = Config::from_lookup(lookup(&[("DATABASE_URL", "postgres://x")])).unwrap();
        assert_eq!(config.musicnn_model_dir, None);
        assert!(!config.disable_musicnn);
    }

    #[test]
    fn defaults_match_upstream() {
        let config = Config::from_lookup(lookup(&[("DATABASE_URL", "postgres://x")])).unwrap();
        assert_eq!(config.bind_addr, "0.0.0.0:3030".parse().unwrap());
        assert_eq!(config.data_dir, PathBuf::from("/data"));
        assert_eq!(config.music_path, PathBuf::from("/music"));
        assert_eq!(config.log_level, "info");
        assert_eq!(config.frontend_dir, None);
        assert!(config.allowed_origins.is_empty());
        assert_eq!(config.session_secret, None);
    }

    #[test]
    fn watch_and_organize_defaults() {
        let config = Config::from_lookup(lookup(&[("DATABASE_URL", "postgres://x")])).unwrap();
        assert_eq!(config.watch_mode, WatchMode::Auto);
        assert_eq!(config.watch_poll_secs, 60);
        assert_eq!(config.organize_target, None);
        assert_eq!(config.transcode_cache_mb, 2048);
    }

    #[test]
    fn artwork_cache_default_512() {
        let config = Config::from_lookup(lookup(&[("DATABASE_URL", "postgres://x")])).unwrap();
        assert_eq!(config.artwork_cache_mb, 512);
        let config = Config::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://x"),
            ("KIMA_ARTWORK_CACHE_MB", "64"),
        ]))
        .unwrap();
        assert_eq!(config.artwork_cache_mb, 64);
        assert!(matches!(
            Config::from_lookup(lookup(&[
                ("DATABASE_URL", "postgres://x"),
                ("KIMA_ARTWORK_CACHE_MB", "lots"),
            ])),
            Err(ConfigError::Invalid {
                name: "KIMA_ARTWORK_CACHE_MB",
                ..
            })
        ));
    }

    #[test]
    fn config_default_and_off() {
        let cron = |extra: &[(&str, &str)]| {
            let mut pairs = vec![("DATABASE_URL", "postgres://x")];
            pairs.extend_from_slice(extra);
            Config::from_lookup(lookup(&pairs))
                .unwrap()
                .library_sync_cron
        };
        assert_eq!(cron(&[]), Some("0 4 * * *".to_string()));
        assert_eq!(cron(&[("KIMA_LIBRARY_SYNC_CRON", "off")]), None);
        assert_eq!(cron(&[("KIMA_LIBRARY_SYNC_CRON", " OFF ")]), None);
        assert_eq!(
            cron(&[("KIMA_LIBRARY_SYNC_CRON", "*/15 * * * *")]),
            Some("*/15 * * * *".to_string())
        );
    }

    #[test]
    fn identify_default_on_and_off() {
        let off = |extra: &[(&str, &str)]| {
            let mut pairs = vec![("DATABASE_URL", "postgres://x")];
            pairs.extend_from_slice(extra);
            Config::from_lookup(lookup(&pairs))
                .unwrap()
                .identify_env_off
        };
        assert!(!off(&[]));
        assert!(!off(&[("KIMA_IDENTIFY", "on")]));
        assert!(!off(&[("KIMA_IDENTIFY", " ON ")]));
        assert!(off(&[("KIMA_IDENTIFY", "off")]));
        assert!(off(&[("KIMA_IDENTIFY", "Off")]));
    }

    #[test]
    fn contact_default() {
        let config = Config::from_lookup(lookup(&[("DATABASE_URL", "postgres://x")])).unwrap();
        assert_eq!(config.contact, "https://github.com/d1mmu-dev/kima-rs-hub");
        assert_eq!(config.musicbrainz_base_url, "https://musicbrainz.org/ws/2");
        let config = Config::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://x"),
            ("KIMA_CONTACT", " ops@example.org "),
        ]))
        .unwrap();
        assert_eq!(config.contact, "ops@example.org");
    }

    #[test]
    fn musicbrainz_base_trailing_slash_stripped() {
        let base = |v: &str| {
            Config::from_lookup(lookup(&[
                ("DATABASE_URL", "postgres://x"),
                ("MUSICBRAINZ_BASE_URL", v),
            ]))
            .map(|c| c.musicbrainz_base_url)
        };
        assert_eq!(
            base("http://mb.lan:5000/ws/2/"),
            Ok("http://mb.lan:5000/ws/2".into())
        );
        assert_eq!(
            base("https://musicbrainz.org/ws/2"),
            Ok("https://musicbrainz.org/ws/2".into())
        );
        for bad in ["mb.lan/ws/2", "ftp://mb.lan", "https://", "http:///"] {
            assert_eq!(
                base(bad),
                Err(ConfigError::Invalid {
                    name: "MUSICBRAINZ_BASE_URL",
                    value: bad.into()
                })
            );
        }
    }

    #[test]
    fn fingerprint_workers_default_half_cores() {
        let config = Config::from_lookup(lookup(&[("DATABASE_URL", "postgres://x")])).unwrap();
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        assert_eq!(config.fingerprint_workers, (cores / 2).max(1));
        let config = Config::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://x"),
            ("KIMA_FINGERPRINT_WORKERS", " 3 "),
        ]))
        .unwrap();
        assert_eq!(config.fingerprint_workers, 3);
    }

    #[test]
    fn fingerprint_workers_zero_invalid() {
        for bad in ["0", "-1", "two", "1.5"] {
            assert_eq!(
                Config::from_lookup(lookup(&[
                    ("DATABASE_URL", "postgres://x"),
                    ("KIMA_FINGERPRINT_WORKERS", bad)
                ])),
                Err(ConfigError::Invalid {
                    name: "KIMA_FINGERPRINT_WORKERS",
                    value: bad.into()
                })
            );
        }
    }

    #[test]
    fn acoustid_disabled_parses() {
        let disabled = |extra: &[(&str, &str)]| {
            let mut pairs = vec![("DATABASE_URL", "postgres://x")];
            pairs.extend_from_slice(extra);
            Config::from_lookup(lookup(&pairs))
                .unwrap()
                .acoustid_disabled
        };
        assert!(!disabled(&[]));
        assert!(disabled(&[("ACOUSTID_DISABLED", "true")]));
        assert!(disabled(&[("ACOUSTID_DISABLED", " TRUE ")]));
        assert!(!disabled(&[("ACOUSTID_DISABLED", "false")]));
        assert!(!disabled(&[("ACOUSTID_DISABLED", "1")]));
        assert!(!disabled(&[("ACOUSTID_DISABLED", "yes")]));
    }

    #[test]
    fn acoustid_client_key_is_read_trimmed_and_never_printed() {
        let key = |extra: &[(&str, &str)]| {
            let mut pairs = vec![("DATABASE_URL", "postgres://x")];
            pairs.extend_from_slice(extra);
            Config::from_lookup(lookup(&pairs)).unwrap()
        };
        assert_eq!(key(&[]).acoustid_client_key, None);
        assert_eq!(
            key(&[("ACOUSTID_CLIENT_KEY", "  ")]).acoustid_client_key,
            None
        );
        let config = key(&[
            ("ACOUSTID_CLIENT_KEY", " abc123XYZ "),
            ("SESSION_SECRET", "sess-secret-value"),
            ("SETTINGS_ENCRYPTION_KEY", "enc-secret-value"),
        ]);
        assert_eq!(config.acoustid_client_key.as_deref(), Some("abc123XYZ"));
        let shown = format!("{config:?}");
        for secret in ["abc123XYZ", "sess-secret-value", "enc-secret-value"] {
            assert!(!shown.contains(secret), "{shown}");
        }
        assert!(
            shown.contains("acoustid_client_key: Some(\"..\")"),
            "{shown}"
        );
    }

    #[test]
    fn fanart_api_key_is_read_trimmed_and_never_printed() {
        let key = |extra: &[(&str, &str)]| {
            let mut pairs = vec![("DATABASE_URL", "postgres://x")];
            pairs.extend_from_slice(extra);
            Config::from_lookup(lookup(&pairs)).unwrap()
        };
        assert_eq!(key(&[]).fanart_api_key, None);
        assert_eq!(key(&[("FANART_API_KEY", " ")]).fanart_api_key, None);
        let config = key(&[("FANART_API_KEY", " fanart-secret-value ")]);
        assert_eq!(
            config.fanart_api_key.as_deref(),
            Some("fanart-secret-value")
        );
        let shown = format!("{config:?} {config:#?}");
        assert!(!shown.contains("fanart-secret-value"), "{shown}");
        assert!(shown.contains("fanart_api_key: Some(\"..\")"), "{shown}");
    }

    #[test]
    fn identify_invalid_fails() {
        assert_eq!(
            Config::from_lookup(lookup(&[
                ("DATABASE_URL", "postgres://x"),
                ("KIMA_IDENTIFY", "maybe")
            ])),
            Err(ConfigError::Invalid {
                name: "KIMA_IDENTIFY",
                value: "maybe".into()
            })
        );
    }

    #[test]
    fn watch_mode_parses() {
        let mode = |v: &str| {
            Config::from_lookup(lookup(&[
                ("DATABASE_URL", "postgres://x"),
                ("KIMA_WATCH_MODE", v),
            ]))
            .unwrap()
            .watch_mode
        };
        assert_eq!(mode("poll"), WatchMode::Poll);
        assert_eq!(mode("off"), WatchMode::Off);
        assert_eq!(mode("AUTO"), WatchMode::Auto);
    }

    #[test]
    fn bad_watch_mode_is_invalid() {
        assert_eq!(
            Config::from_lookup(lookup(&[
                ("DATABASE_URL", "postgres://x"),
                ("KIMA_WATCH_MODE", "sometimes")
            ])),
            Err(ConfigError::Invalid {
                name: "KIMA_WATCH_MODE",
                value: "sometimes".into()
            })
        );
    }

    #[test]
    fn poll_secs_must_be_positive() {
        for bad in ["0", "-1", "soon"] {
            assert_eq!(
                Config::from_lookup(lookup(&[
                    ("DATABASE_URL", "postgres://x"),
                    ("KIMA_WATCH_POLL_SECS", bad)
                ])),
                Err(ConfigError::Invalid {
                    name: "KIMA_WATCH_POLL_SECS",
                    value: bad.into()
                })
            );
        }
    }

    #[test]
    fn organize_target_and_cache_are_read() {
        let config = Config::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://x"),
            ("KIMA_ORGANIZE_TARGET", "/organized"),
            ("KIMA_TRANSCODE_CACHE_MB", "512"),
        ]))
        .unwrap();
        assert_eq!(config.organize_target, Some(PathBuf::from("/organized")));
        assert_eq!(config.transcode_cache_mb, 512);
    }

    #[test]
    fn public_url_trailing_slash_stripped() {
        let url = |v: &str| {
            Config::from_lookup(lookup(&[
                ("DATABASE_URL", "postgres://x"),
                ("KIMA_PUBLIC_URL", v),
            ]))
            .unwrap()
            .public_url
        };
        assert_eq!(
            url("https://kima.example/"),
            Some("https://kima.example".into())
        );
        assert_eq!(
            url("http://10.0.0.2:3030//"),
            Some("http://10.0.0.2:3030".into())
        );
        assert_eq!(
            url("https://k.example/base"),
            Some("https://k.example/base".into())
        );
        let unset = Config::from_lookup(lookup(&[("DATABASE_URL", "postgres://x")])).unwrap();
        assert_eq!(unset.public_url, None);
    }

    #[test]
    fn public_url_without_scheme_invalid() {
        for bad in ["kima.example", "ftp://kima.example", "/", "https://"] {
            assert_eq!(
                Config::from_lookup(lookup(&[
                    ("DATABASE_URL", "postgres://x"),
                    ("KIMA_PUBLIC_URL", bad)
                ])),
                Err(ConfigError::Invalid {
                    name: "KIMA_PUBLIC_URL",
                    value: bad.into()
                })
            );
        }
    }

    #[test]
    fn port_must_be_a_number() {
        assert_eq!(
            Config::from_lookup(lookup(&[
                ("DATABASE_URL", "postgres://x"),
                ("PORT", "eighty")
            ])),
            Err(ConfigError::Invalid {
                name: "PORT",
                value: "eighty".into()
            })
        );
    }

    #[test]
    fn allowed_origins_are_split_and_trimmed() {
        let config = Config::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://x"),
            ("ALLOWED_ORIGINS", " https://a.example , ,https://b.example"),
        ]))
        .unwrap();
        assert_eq!(
            config.allowed_origins,
            vec!["https://a.example", "https://b.example"]
        );
    }

    #[test]
    fn explicit_values_override_defaults() {
        let config = Config::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://x"),
            ("PORT", "3111"),
            ("KIMA_BIND_HOST", "127.0.0.1"),
            ("KIMA_DATA_DIR", "/srv/kima"),
            ("KIMA_FRONTEND_DIR", "/srv/web"),
            ("SESSION_SECRET", "s3cret"),
        ]))
        .unwrap();
        assert_eq!(config.bind_addr, "127.0.0.1:3111".parse().unwrap());
        assert_eq!(config.data_dir, PathBuf::from("/srv/kima"));
        assert_eq!(config.frontend_dir, Some(PathBuf::from("/srv/web")));
        assert_eq!(config.session_secret.as_deref(), Some("s3cret"));
    }

    #[test]
    fn web_ssr_is_off_unless_a_build_dir_is_set() {
        let config = Config::from_lookup(lookup(&[("DATABASE_URL", "postgres://x")])).unwrap();
        assert_eq!(config.web_build_dir, None);
        assert_eq!(config.web_ssr_port, DEFAULT_WEB_SSR_PORT);

        let config = Config::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://x"),
            ("KIMA_WEB_BUILD_DIR", "/srv/kima/web-ssr/build"),
            ("KIMA_WEB_SSR_PORT", "4555"),
        ]))
        .unwrap();
        assert_eq!(
            config.web_build_dir,
            Some(PathBuf::from("/srv/kima/web-ssr/build"))
        );
        assert_eq!(config.web_ssr_port, 4555);
    }

    #[test]
    fn web_ssr_port_must_be_a_real_port() {
        for bad in ["0", "70000", "x", "-1"] {
            assert_eq!(
                Config::from_lookup(lookup(&[
                    ("DATABASE_URL", "postgres://x"),
                    ("KIMA_WEB_SSR_PORT", bad),
                ])),
                Err(ConfigError::Invalid {
                    name: "KIMA_WEB_SSR_PORT",
                    value: bad.into()
                }),
                "{bad}"
            );
        }
    }
}
