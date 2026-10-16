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
