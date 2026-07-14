use std::net::SocketAddr;

use roosty_core::{Result, RoostyError};
use url::Url;

const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:4000";
const DEFAULT_MEDIA_ROOT: &str = "./media";
const DEFAULT_OBJECT_STORAGE_BACKEND: &str = "local";
const DEFAULT_REGISTRATION_MODE: &str = "closed";
const DEFAULT_WORKER_CONCURRENCY: &str = "4";

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct Config {
    pub database_url: String,
    pub public_base_url: Url,
    pub listen_addr: SocketAddr,
    pub infra_listen_addr: Option<SocketAddr>,
    pub session_secret: String,
    pub token_pepper: String,
    pub object_storage_backend: String,
    pub media_root: String,
    pub registration_mode: String,
    pub federation_enabled: bool,
    /// Secret used to encrypt persisted local actor private keys.
    pub federation_key_encryption_secret: Option<String>,
    /// Remote domains permitted for discovery and delivery. The `*` entry permits all domains.
    pub federation_allowed_domains: Vec<String>,
    /// Exact remote domains prohibited for discovery and delivery, including when `*` is allowed.
    pub federation_blocked_domains: Vec<String>,
    /// Maximum age for retrying a failed federation delivery job.
    pub federation_delivery_max_age: time::Duration,
    /// Retention period for successfully fetched remote media.
    pub remote_media_cache_ttl: time::Duration,
    /// Maximum bytes accepted from one remote media response.
    pub remote_media_max_bytes: u64,
    /// Maximum remote media downloads this worker runs concurrently.
    pub remote_media_fetch_concurrency: usize,
    /// Number of durable job loops to run in this process; zero in configuration uses available CPUs.
    pub worker_concurrency: usize,
    pub instance_name: String,
    pub instance_description: Option<String>,
}

impl Config {
    pub fn from_env(listen_override: Option<SocketAddr>) -> Result<Self> {
        let listen_addr = match listen_override {
            Some(listen) => listen,
            None => parse_env("ROOSTY_LISTEN_ADDR", DEFAULT_LISTEN_ADDR)?,
        };

        let public_base_url: Url =
            required_env("ROOSTY_PUBLIC_BASE_URL")?
                .parse()
                .map_err(|error| {
                    RoostyError::Configuration(format!(
                        "ROOSTY_PUBLIC_BASE_URL is invalid: {error}"
                    ))
                })?;
        let federation_enabled = optional_bool_env("ROOSTY_FEDERATION_ENABLED")?.unwrap_or(false);
        let federation_key_encryption_secret =
            optional_env("ROOSTY_FEDERATION_KEY_ENCRYPTION_SECRET");
        let federation_allowed_domains = optional_domain_list("ROOSTY_FEDERATION_ALLOWED_DOMAINS")?;
        let federation_blocked_domains = optional_domain_list("ROOSTY_FEDERATION_BLOCKED_DOMAINS")?;
        let federation_delivery_max_age =
            optional_humantime_duration_env("ROOSTY_FEDERATION_DELIVERY_MAX_AGE", "7d")?;
        let remote_media_cache_ttl =
            optional_humantime_duration_env("ROOSTY_REMOTE_MEDIA_CACHE_TTL", "30d")?;
        let remote_media_max_bytes =
            optional_bytesize_env("ROOSTY_REMOTE_MEDIA_MAX_BYTES", "40MiB")?;
        let remote_media_fetch_concurrency =
            parse_env("ROOSTY_REMOTE_MEDIA_FETCH_CONCURRENCY", "5")?;
        if remote_media_fetch_concurrency == 0 {
            return Err(RoostyError::Configuration(
                "ROOSTY_REMOTE_MEDIA_FETCH_CONCURRENCY must be positive".to_owned(),
            ));
        }
        let worker_concurrency = resolve_worker_concurrency(parse_env(
            "ROOSTY_WORKER_CONCURRENCY",
            DEFAULT_WORKER_CONCURRENCY,
        )?)?;
        if federation_enabled {
            if public_base_url.scheme() != "https" || public_base_url.host_str().is_none() {
                return Err(RoostyError::Configuration(
                    "ROOSTY_PUBLIC_BASE_URL must be an absolute HTTPS URL when federation is enabled".to_owned(),
                ));
            }
            let Some(secret) = federation_key_encryption_secret.as_deref() else {
                return Err(RoostyError::Configuration(
                    "ROOSTY_FEDERATION_KEY_ENCRYPTION_SECRET is required when federation is enabled"
                        .to_owned(),
                ));
            };
            if secret.len() < 32 {
                return Err(RoostyError::Configuration(
                    "ROOSTY_FEDERATION_KEY_ENCRYPTION_SECRET must be at least 32 bytes".to_owned(),
                ));
            }
            if federation_allowed_domains.is_empty() {
                return Err(RoostyError::Configuration(
                    "ROOSTY_FEDERATION_ALLOWED_DOMAINS must contain at least one domain when federation is enabled".to_owned(),
                ));
            }
        }

        Ok(Self {
            database_url: required_env("ROOSTY_DATABASE_URL")?,
            public_base_url,
            listen_addr,
            infra_listen_addr: optional_parse_env("ROOSTY_INFRA_LISTEN_ADDR")?,
            session_secret: required_secret("ROOSTY_SESSION_SECRET")?,
            token_pepper: required_secret("ROOSTY_TOKEN_PEPPER")?,
            object_storage_backend: optional_env("ROOSTY_OBJECT_STORAGE_BACKEND")
                .unwrap_or_else(|| DEFAULT_OBJECT_STORAGE_BACKEND.to_owned()),
            media_root: optional_env("ROOSTY_MEDIA_ROOT")
                .unwrap_or_else(|| DEFAULT_MEDIA_ROOT.to_owned()),
            registration_mode: optional_env("ROOSTY_REGISTRATION_MODE")
                .unwrap_or_else(|| DEFAULT_REGISTRATION_MODE.to_owned()),
            federation_enabled,
            federation_key_encryption_secret,
            federation_allowed_domains,
            federation_blocked_domains,
            federation_delivery_max_age,
            remote_media_cache_ttl,
            remote_media_max_bytes,
            remote_media_fetch_concurrency,
            worker_concurrency,
            instance_name: required_env("ROOSTY_INSTANCE_NAME")?,
            instance_description: optional_env("ROOSTY_INSTANCE_DESCRIPTION"),
        })
    }

    /// Return whether the configured federation policy permits a remote DNS domain.
    ///
    /// A wildcard allow-list entry permits every domain, but an explicit block always wins.
    pub fn federation_domain_is_allowed(&self, domain: &str) -> bool {
        let domain = domain.to_ascii_lowercase();
        self.federation_allowed_domains
            .iter()
            .any(|allowed| allowed == "*" || allowed == &domain)
            && !self
                .federation_blocked_domains
                .iter()
                .any(|blocked| blocked == &domain)
    }
}

fn optional_bytesize_env(name: &str, default: &str) -> Result<u64> {
    let value = optional_env(name).unwrap_or_else(|| default.to_owned());
    value
        .parse::<bytesize::ByteSize>()
        .map(|size| size.as_u64())
        .map_err(|_| {
            RoostyError::Configuration(format!(
                "{name} must be a human-readable byte size, such as 40MiB"
            ))
        })
}

/// Resolve zero worker slots to the number of logical CPUs available to this process.
fn resolve_worker_concurrency(configured: usize) -> Result<usize> {
    if configured != 0 {
        return Ok(configured);
    }

    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .map_err(|error| {
            RoostyError::Configuration(format!(
                "could not determine available worker CPUs: {error}"
            ))
        })
}

fn optional_humantime_duration_env(name: &str, default: &str) -> Result<time::Duration> {
    let value = optional_env(name).unwrap_or_else(|| default.to_owned());
    let duration = humantime::parse_duration(&value).map_err(|_| {
        RoostyError::Configuration(format!(
            "{name} must be a positive human-readable duration, such as 7d or 12h"
        ))
    })?;
    if duration.is_zero() {
        return Err(RoostyError::Configuration(format!(
            "{name} must be a positive human-readable duration"
        )));
    }
    time::Duration::try_from(duration)
        .map_err(|_| RoostyError::Configuration(format!("{name} is too large")))
}

/// Parse a comma-separated list of DNS host names or the `*` federation wildcard.
fn optional_domain_list(name: &str) -> Result<Vec<String>> {
    optional_env(name)
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|domain| !domain.is_empty())
                .map(|domain| {
                    if domain.contains('/')
                        || domain.contains('@')
                        || domain.parse::<std::net::IpAddr>().is_ok()
                    {
                        return Err(RoostyError::Configuration(format!(
                            "{name} contains an invalid domain"
                        )));
                    }
                    Ok(domain.to_ascii_lowercase())
                })
                .collect()
        })
        .transpose()
        .map(|domains: Option<Vec<String>>| domains.unwrap_or_default())
}

pub fn database_url_from_env() -> Result<String> {
    required_env("ROOSTY_DATABASE_URL")
}

fn required_env(name: &str) -> Result<String> {
    let value = std::env::var(name)
        .map_err(|_| RoostyError::Configuration(format!("{name} is required")))?;
    if value.trim().is_empty() {
        return Err(RoostyError::Configuration(format!(
            "{name} must not be empty"
        )));
    }

    Ok(value)
}

fn required_secret(name: &str) -> Result<String> {
    let value = required_env(name)?;
    if value.len() < 32 {
        return Err(RoostyError::Configuration(format!(
            "{name} must be at least 32 bytes"
        )));
    }

    Ok(value)
}

fn optional_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn parse_env<T>(name: &str, default: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    optional_env(name)
        .unwrap_or_else(|| default.to_owned())
        .parse()
        .map_err(|error| RoostyError::Configuration(format!("{name} is invalid: {error}")))
}

fn optional_parse_env<T>(name: &str) -> Result<Option<T>>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    optional_env(name)
        .map(|value| {
            value
                .parse()
                .map_err(|error| RoostyError::Configuration(format!("{name} is invalid: {error}")))
        })
        .transpose()
}

fn optional_bool_env(name: &str) -> Result<Option<bool>> {
    optional_env(name)
        .map(|value| match value.as_str() {
            "1" | "true" | "TRUE" | "yes" | "YES" => Ok(true),
            "0" | "false" | "FALSE" | "no" | "NO" => Ok(false),
            _ => Err(RoostyError::Configuration(format!(
                "{name} must be a boolean"
            ))),
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_boolean_values() {
        assert!(optional_bool_value("true").unwrap());
        assert!(!optional_bool_value("0").unwrap());
        assert!(optional_bool_value("sometimes").is_err());
    }

    #[test]
    fn federation_wildcard_allows_domains_unless_explicitly_blocked() {
        let config = Config {
            database_url: "postgres://unused".to_owned(),
            public_base_url: "https://roosty.example".parse().unwrap(),
            listen_addr: "127.0.0.1:4000".parse().unwrap(),
            infra_listen_addr: None,
            session_secret: "test-session-secret".to_owned(),
            token_pepper: "test-token-pepper".to_owned(),
            object_storage_backend: "local".to_owned(),
            media_root: "./media".to_owned(),
            registration_mode: "closed".to_owned(),
            federation_enabled: true,
            federation_key_encryption_secret: Some("test-federation-secret".to_owned()),
            federation_allowed_domains: vec!["*".to_owned()],
            federation_blocked_domains: vec!["blocked.example".to_owned()],
            federation_delivery_max_age: time::Duration::days(7),
            remote_media_cache_ttl: time::Duration::days(30),
            remote_media_max_bytes: 40 * 1024 * 1024,
            remote_media_fetch_concurrency: 5,
            worker_concurrency: 4,
            instance_name: "Roosty Test".to_owned(),
            instance_description: None,
        };

        assert!(config.federation_domain_is_allowed("remote.example"));
        assert!(config.federation_domain_is_allowed("REMOTE.EXAMPLE"));
        assert!(!config.federation_domain_is_allowed("blocked.example"));
    }

    #[test]
    fn resolves_zero_worker_concurrency_to_available_cpus() {
        assert_eq!(resolve_worker_concurrency(3).unwrap(), 3);
        assert_eq!(
            resolve_worker_concurrency(0).unwrap(),
            std::thread::available_parallelism().unwrap().get()
        );
    }

    fn optional_bool_value(value: &str) -> Result<bool> {
        match value {
            "1" | "true" | "TRUE" | "yes" | "YES" => Ok(true),
            "0" | "false" | "FALSE" | "no" | "NO" => Ok(false),
            _ => Err(RoostyError::Configuration(
                "ROOSTY_FEDERATION_ENABLED must be a boolean".to_owned(),
            )),
        }
    }
}
