use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub database_url: String,
    pub data_dir: PathBuf,
    pub music_path: PathBuf,
    pub bind_addr: SocketAddr,
    pub frontend_dir: Option<PathBuf>,
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
}
