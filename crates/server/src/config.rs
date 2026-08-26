use std::{
    env,
    fmt::Display as FmtDisplay,
    net::{IpAddr, SocketAddr},
    num::NonZeroUsize,
    result::Result as StdResult,
    str::FromStr,
    thread,
    time::Duration,
};

use ipnet::IpNet;
use roosty_core::{Result, RoostyError};
use strum::{Display, EnumString};
use url::Url;

const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:4000";
const DEFAULT_MEDIA_ROOT: &str = "./media";
const DEFAULT_OBJECT_STORAGE_BACKEND: &str = "local";
const DEFAULT_REGISTRATION_MODE: &str = "closed";
const DEFAULT_WORKER_CONCURRENCY: &str = "4";
const DEFAULT_ACCOUNT_SUGGESTIONS_REFRESH_INTERVAL: &str = "24h";
const DEFAULT_SUCCESSFUL_JOB_RETENTION: &str = "24h";
const DEFAULT_PERMANENTLY_FAILED_JOB_RETENTION: &str = "30d";

/// Operator-configurable policy limits for future publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduledStatusConfig {
    pub minimum_offset: time::Duration,
    pub total_limit: u64,
    pub daily_limit: u64,
}

impl Default for ScheduledStatusConfig {
    fn default() -> Self {
        Self {
            minimum_offset: time::Duration::minutes(5),
            total_limit: 300,
            daily_limit: 25,
        }
    }
}

impl ScheduledStatusConfig {
    fn from_env() -> Result<Self> {
        let minimum_offset =
            optional_humantime_duration_env("ROOSTY_SCHEDULED_STATUS_MINIMUM_OFFSET", "5m")?;
        let total_limit = parse_env("ROOSTY_SCHEDULED_STATUS_TOTAL_LIMIT", "300")?;
        let daily_limit = parse_env("ROOSTY_SCHEDULED_STATUS_DAILY_LIMIT", "25")?;
        if total_limit == 0 {
            return Err(RoostyError::Configuration(
                "ROOSTY_SCHEDULED_STATUS_TOTAL_LIMIT must be positive".to_owned(),
            ));
        }
        if daily_limit == 0 {
            return Err(RoostyError::Configuration(
                "ROOSTY_SCHEDULED_STATUS_DAILY_LIMIT must be positive".to_owned(),
            ));
        }
        Ok(Self {
            minimum_offset,
            total_limit,
            daily_limit,
        })
    }
}

/// Configured storage implementation for locally managed media.
#[derive(Clone, Copy, Debug, Display, EnumString, Eq, PartialEq)]
#[strum(serialize_all = "snake_case")]
pub enum ObjectStorageBackend {
    Local,
}

/// Operator policy controlling whether public account registration is advertised.
#[derive(Clone, Copy, Debug, Display, EnumString, Eq, PartialEq)]
#[strum(serialize_all = "snake_case")]
pub enum RegistrationMode {
    Closed,
    Open,
    Approval,
}

/// Durable registration-attempt limits and proxy-aware client addressing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistrationRateLimitConfig {
    pub burst_limit: u64,
    pub burst_window: Duration,
    pub daily_limit: u64,
    pub daily_window: Duration,
    pub ipv6_prefix_length: u8,
    pub trusted_proxy_cidrs: Vec<IpNet>,
}

impl Default for RegistrationRateLimitConfig {
    fn default() -> Self {
        Self {
            burst_limit: 5,
            burst_window: Duration::from_secs(30 * 60),
            daily_limit: 20,
            daily_window: Duration::from_secs(24 * 60 * 60),
            ipv6_prefix_length: 64,
            trusted_proxy_cidrs: Vec::new(),
        }
    }
}

impl RegistrationRateLimitConfig {
    fn from_env() -> Result<Self> {
        let value = Self {
            burst_limit: parse_env("ROOSTY_REGISTRATION_BURST_LIMIT", "5")?,
            burst_window: nonzero_duration_env("ROOSTY_REGISTRATION_BURST_WINDOW", "30m")?,
            daily_limit: parse_env("ROOSTY_REGISTRATION_DAILY_LIMIT", "20")?,
            daily_window: nonzero_duration_env("ROOSTY_REGISTRATION_DAILY_WINDOW", "24h")?,
            ipv6_prefix_length: parse_env("ROOSTY_REGISTRATION_IPV6_PREFIX_LENGTH", "64")?,
            trusted_proxy_cidrs: optional_env("ROOSTY_TRUSTED_PROXY_CIDRS")
                .filter(|value| !value.trim().is_empty())
                .map(|value| {
                    value
                        .split(',')
                        .map(|cidr| {
                            cidr.trim().parse().map_err(|error| {
                                RoostyError::Configuration(format!(
                                    "ROOSTY_TRUSTED_PROXY_CIDRS contains an invalid CIDR: {error}"
                                ))
                            })
                        })
                        .collect()
                })
                .transpose()?
                .unwrap_or_default(),
        };
        if value.burst_limit == 0 || value.daily_limit == 0 {
            return Err(RoostyError::Configuration(
                "registration rate limits must be positive".to_owned(),
            ));
        }
        if value.daily_window <= value.burst_window {
            return Err(RoostyError::Configuration("ROOSTY_REGISTRATION_DAILY_WINDOW must be greater than ROOSTY_REGISTRATION_BURST_WINDOW".to_owned()));
        }
        if value.daily_limit < value.burst_limit {
            return Err(RoostyError::Configuration(
                "ROOSTY_REGISTRATION_DAILY_LIMIT must be at least ROOSTY_REGISTRATION_BURST_LIMIT"
                    .to_owned(),
            ));
        }
        if !(1..=128).contains(&value.ipv6_prefix_length) {
            return Err(RoostyError::Configuration(
                "ROOSTY_REGISTRATION_IPV6_PREFIX_LENGTH must be between 1 and 128".to_owned(),
            ));
        }
        Ok(value)
    }
}

/// Boolean configuration whose absent value enables the feature.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DefaultEnabled(bool);

impl DefaultEnabled {
    pub const fn is_enabled(self) -> bool {
        self.0
    }

    fn resolve(
        override_value: Option<Self>,
        environment_value: impl FnOnce() -> Result<Option<Self>>,
    ) -> Result<Self> {
        match override_value {
            Some(value) => Ok(value),
            None => Ok(environment_value()?.unwrap_or_default()),
        }
    }
}

impl Default for DefaultEnabled {
    fn default() -> Self {
        Self(true)
    }
}

impl From<bool> for DefaultEnabled {
    fn from(value: bool) -> Self {
        Self(value)
    }
}

impl FromStr for DefaultEnabled {
    type Err = <bool as FromStr>::Err;

    fn from_str(value: &str) -> StdResult<Self, Self::Err> {
        value.parse().map(Self)
    }
}

/// Per-process limits and timers for Mastodon-compatible streaming sockets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamingConfig {
    pub max_connections: usize,
    pub send_timeout: Duration,
    pub ping_interval: Duration,
    pub idle_timeout: Duration,
    pub event_retention: Duration,
}

impl Default for StreamingConfig {
    fn default() -> Self {
        Self {
            max_connections: 1_000,
            send_timeout: Duration::from_secs(10),
            ping_interval: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(90),
            event_retention: Duration::from_secs(60 * 60),
        }
    }
}

impl StreamingConfig {
    fn from_env() -> Result<Self> {
        let max_connections = parse_env("ROOSTY_STREAMING_MAX_CONNECTIONS", "1000")?;
        let send_timeout = nonzero_duration_env("ROOSTY_STREAMING_SEND_TIMEOUT", "10s")?;
        let ping_interval = nonzero_duration_env("ROOSTY_STREAMING_PING_INTERVAL", "30s")?;
        let idle_timeout = nonzero_duration_env("ROOSTY_STREAMING_IDLE_TIMEOUT", "90s")?;
        let event_retention = nonzero_duration_env("ROOSTY_STREAMING_EVENT_RETENTION", "1h")?;
        Self::validated(
            max_connections,
            send_timeout,
            ping_interval,
            idle_timeout,
            event_retention,
        )
    }

    fn validated(
        max_connections: usize,
        send_timeout: Duration,
        ping_interval: Duration,
        idle_timeout: Duration,
        event_retention: Duration,
    ) -> Result<Self> {
        if max_connections == 0 {
            return Err(RoostyError::Configuration(
                "ROOSTY_STREAMING_MAX_CONNECTIONS must be positive".to_owned(),
            ));
        }
        for (name, duration) in [
            ("ROOSTY_STREAMING_SEND_TIMEOUT", send_timeout),
            ("ROOSTY_STREAMING_PING_INTERVAL", ping_interval),
            ("ROOSTY_STREAMING_IDLE_TIMEOUT", idle_timeout),
            ("ROOSTY_STREAMING_EVENT_RETENTION", event_retention),
        ] {
            if duration.is_zero() {
                return Err(RoostyError::Configuration(format!(
                    "{name} must be a non-zero humantime duration, such as 10s, 90s, or 1h"
                )));
            }
        }
        if idle_timeout <= ping_interval {
            return Err(RoostyError::Configuration(
                "ROOSTY_STREAMING_IDLE_TIMEOUT must be greater than ROOSTY_STREAMING_PING_INTERVAL"
                    .to_owned(),
            ));
        }

        Ok(Self {
            max_connections,
            send_timeout,
            ping_interval,
            idle_timeout,
            event_retention,
        })
    }
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct Config {
    pub database_url: String,
    pub public_base_url: Url,
    pub listen_addr: SocketAddr,
    pub infra_listen_addr: Option<SocketAddr>,
    pub session_secret: String,
    pub token_pepper: String,
    /// Base64-encoded PKCS#8 P-256 private key used for VAPID.
    pub vapid_private_key: Option<String>,
    pub object_storage_backend: ObjectStorageBackend,
    pub media_root: String,
    pub registration_mode: RegistrationMode,
    pub registration_rate_limit: RegistrationRateLimitConfig,
    /// Whether public pages may be indexed and advertised to search crawlers.
    pub search_indexing_enabled: DefaultEnabled,
    pub federation_enabled: bool,
    /// Secret used to encrypt persisted local actor private keys.
    pub federation_key_encryption_secret: Option<String>,
    /// Remote domains permitted for discovery and delivery. The `*` entry permits all domains.
    pub federation_allowed_domains: Vec<String>,
    /// Maximum age for retrying a failed federation delivery job.
    pub federation_delivery_max_age: time::Duration,
    /// Age at which both active actor-key algorithms are rotated.
    pub federation_key_rotation_interval: time::Duration,
    /// Publication overlap retained for retiring actor keys.
    pub federation_key_overlap: time::Duration,
    /// Retention period for successfully fetched remote media.
    pub remote_media_cache_ttl: time::Duration,
    /// Maximum bytes accepted from one remote media response.
    pub remote_media_max_bytes: u64,
    /// Maximum remote media downloads this worker runs concurrently.
    pub remote_media_fetch_concurrency: usize,
    /// Maximum preview-card downloads this process runs concurrently.
    pub preview_card_fetch_concurrency: usize,
    /// Number of durable job loops to run in this process; zero in configuration uses available CPUs.
    pub worker_concurrency: usize,
    /// Retention period for successfully completed durable jobs.
    pub successful_job_retention: time::Duration,
    /// Retention period for permanently failed durable jobs and their diagnostics.
    pub permanently_failed_job_retention: time::Duration,
    /// Shared cadence for eventually consistent trend refreshes.
    pub trends_refresh_interval: time::Duration,
    /// Per-process cadence for refreshing global account-suggestion scores.
    pub account_suggestions_refresh_interval: time::Duration,
    pub scheduled_statuses: ScheduledStatusConfig,
    pub streaming: StreamingConfig,
    pub instance_name: String,
    pub instance_description: Option<String>,
}

impl Config {
    pub fn from_env(
        listen_override: Option<SocketAddr>,
        search_indexing_override: Option<DefaultEnabled>,
    ) -> Result<Self> {
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
        let vapid_private_key = optional_env("ROOSTY_VAPID_PRIVATE_KEY");
        if let Some(key) = vapid_private_key.as_deref() {
            let subject = public_base_url.origin().ascii_serialization();
            roosty_web_push::VapidIdentity::from_base64_pkcs8(key, subject).map_err(|error| {
                RoostyError::Configuration(format!("ROOSTY_VAPID_PRIVATE_KEY is invalid: {error}"))
            })?;
        }
        let federation_enabled = optional_bool_env("ROOSTY_FEDERATION_ENABLED")?.unwrap_or(false);
        let federation_key_encryption_secret =
            optional_env("ROOSTY_FEDERATION_KEY_ENCRYPTION_SECRET");
        let federation_allowed_domains = optional_domain_list("ROOSTY_FEDERATION_ALLOWED_DOMAINS")?;
        let federation_delivery_max_age =
            optional_humantime_duration_env("ROOSTY_FEDERATION_DELIVERY_MAX_AGE", "7d")?;
        let federation_key_rotation_interval =
            optional_humantime_duration_env("ROOSTY_FEDERATION_KEY_ROTATION_INTERVAL", "90d")?;
        let federation_key_overlap =
            optional_humantime_duration_env("ROOSTY_FEDERATION_KEY_OVERLAP", "7d")?;
        if federation_key_rotation_interval <= time::Duration::ZERO
            || federation_key_overlap <= time::Duration::ZERO
        {
            return Err(RoostyError::Configuration(
                "federation key rotation interval and overlap must be positive".to_owned(),
            ));
        }
        if federation_key_overlap >= federation_key_rotation_interval {
            return Err(RoostyError::Configuration(
                "ROOSTY_FEDERATION_KEY_OVERLAP must be shorter than ROOSTY_FEDERATION_KEY_ROTATION_INTERVAL".to_owned(),
            ));
        }
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
        let preview_card_fetch_concurrency = positive_concurrency(
            "ROOSTY_PREVIEW_CARD_FETCH_CONCURRENCY",
            parse_env("ROOSTY_PREVIEW_CARD_FETCH_CONCURRENCY", "5")?,
        )?;
        let worker_concurrency = resolve_worker_concurrency(parse_env(
            "ROOSTY_WORKER_CONCURRENCY",
            DEFAULT_WORKER_CONCURRENCY,
        )?)?;
        let successful_job_retention = optional_humantime_duration_env(
            "ROOSTY_SUCCESSFUL_JOB_RETENTION",
            DEFAULT_SUCCESSFUL_JOB_RETENTION,
        )?;
        let permanently_failed_job_retention = optional_humantime_duration_env(
            "ROOSTY_PERMANENTLY_FAILED_JOB_RETENTION",
            DEFAULT_PERMANENTLY_FAILED_JOB_RETENTION,
        )?;
        let trends_refresh_interval = trends_refresh_interval_env()?;
        let account_suggestions_refresh_interval = optional_humantime_duration_env(
            "ROOSTY_ACCOUNT_SUGGESTIONS_REFRESH_INTERVAL",
            DEFAULT_ACCOUNT_SUGGESTIONS_REFRESH_INTERVAL,
        )?;
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
            vapid_private_key,
            object_storage_backend: parse_env(
                "ROOSTY_OBJECT_STORAGE_BACKEND",
                DEFAULT_OBJECT_STORAGE_BACKEND,
            )?,
            media_root: optional_env("ROOSTY_MEDIA_ROOT")
                .unwrap_or_else(|| DEFAULT_MEDIA_ROOT.to_owned()),
            registration_mode: parse_env("ROOSTY_REGISTRATION_MODE", DEFAULT_REGISTRATION_MODE)?,
            registration_rate_limit: RegistrationRateLimitConfig::from_env()?,
            search_indexing_enabled: DefaultEnabled::resolve(search_indexing_override, || {
                optional_parse_env("ROOSTY_SEARCH_INDEXING_ENABLED")
            })?,
            federation_enabled,
            federation_key_encryption_secret,
            federation_allowed_domains,
            federation_delivery_max_age,
            federation_key_rotation_interval,
            federation_key_overlap,
            remote_media_cache_ttl,
            remote_media_max_bytes,
            remote_media_fetch_concurrency,
            preview_card_fetch_concurrency,
            worker_concurrency,
            successful_job_retention,
            permanently_failed_job_retention,
            trends_refresh_interval,
            account_suggestions_refresh_interval,
            scheduled_statuses: ScheduledStatusConfig::from_env()?,
            streaming: StreamingConfig::from_env()?,
            instance_name: required_env("ROOSTY_INSTANCE_NAME")?,
            instance_description: optional_env("ROOSTY_INSTANCE_DESCRIPTION"),
        })
    }

    /// Return whether the configured federation policy permits a remote DNS domain.
    ///
    /// A wildcard allow-list entry permits every public remote domain.
    pub fn federation_domain_is_allowed(&self, domain: &str) -> bool {
        let domain = domain.to_ascii_lowercase();
        self.federation_allowed_domains
            .iter()
            .any(|allowed| allowed == "*" || allowed == &domain)
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

    thread::available_parallelism()
        .map(NonZeroUsize::get)
        .map_err(|error| {
            RoostyError::Configuration(format!(
                "could not determine available worker CPUs: {error}"
            ))
        })
}

fn optional_humantime_duration_env(name: &str, default: &str) -> Result<time::Duration> {
    let duration = nonzero_duration_env(name, default)?;
    time::Duration::try_from(duration)
        .map_err(|_| RoostyError::Configuration(format!("{name} is too large")))
}

fn trends_refresh_interval_env() -> Result<time::Duration> {
    let duration = optional_humantime_duration_env("ROOSTY_TRENDS_REFRESH_INTERVAL", "5m")?;
    validate_trends_refresh_interval(duration)
}

fn validate_trends_refresh_interval(duration: time::Duration) -> Result<time::Duration> {
    if duration < time::Duration::minutes(1) {
        return Err(RoostyError::Configuration(
            "ROOSTY_TRENDS_REFRESH_INTERVAL must be at least 1m".to_owned(),
        ));
    }
    Ok(duration)
}

fn nonzero_duration_env(name: &str, default: &str) -> Result<Duration> {
    let value = optional_env(name).unwrap_or_else(|| default.to_owned());
    nonzero_duration(name, &value)
}

fn nonzero_duration(name: &str, value: &str) -> Result<Duration> {
    let duration = humantime::parse_duration(value).map_err(|_| {
        RoostyError::Configuration(format!(
            "{name} must be a non-zero humantime duration, such as 10s, 90s, or 1h"
        ))
    })?;
    if duration.is_zero() {
        return Err(RoostyError::Configuration(format!(
            "{name} must be a non-zero humantime duration, such as 10s, 90s, or 1h"
        )));
    }
    Ok(duration)
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
                        || domain.parse::<IpAddr>().is_ok()
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
    let value =
        env::var(name).map_err(|_| RoostyError::Configuration(format!("{name} is required")))?;
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
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

fn parse_env<T>(name: &str, default: &str) -> Result<T>
where
    T: FromStr,
    T::Err: FmtDisplay,
{
    parse_value(
        name,
        &optional_env(name).unwrap_or_else(|| default.to_owned()),
    )
}

fn optional_parse_env<T>(name: &str) -> Result<Option<T>>
where
    T: FromStr,
    T::Err: FmtDisplay,
{
    optional_env(name)
        .map(|value| parse_value(name, &value))
        .transpose()
}

fn optional_bool_env(name: &str) -> Result<Option<bool>> {
    optional_parse_env(name)
}

fn parse_value<T>(name: &str, value: &str) -> Result<T>
where
    T: FromStr,
    T::Err: FmtDisplay,
{
    value
        .parse()
        .map_err(|error| RoostyError::Configuration(format!("{name} is invalid: {error}")))
}

fn positive_concurrency(name: &str, value: usize) -> Result<usize> {
    if value == 0 {
        Err(RoostyError::Configuration(format!(
            "{name} must be positive"
        )))
    } else {
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_boolean_values() {
        assert!(DefaultEnabled::from_str("true").unwrap().is_enabled());
        assert!(!DefaultEnabled::from_str("false").unwrap().is_enabled());
        assert!(DefaultEnabled::from_str("sometimes").is_err());
    }

    #[test]
    fn search_indexing_defaults_enabled_and_cli_value_precedes_environment() {
        assert!(
            DefaultEnabled::resolve(None, || Ok(None))
                .unwrap()
                .is_enabled()
        );
        assert!(
            !DefaultEnabled::resolve(Some(false.into()), || unreachable!())
                .unwrap()
                .is_enabled()
        );
    }

    #[test]
    fn federation_wildcard_allows_every_public_domain() {
        let config = Config {
            database_url: "postgres://unused".to_owned(),
            public_base_url: "https://roosty.example".parse().unwrap(),
            listen_addr: "127.0.0.1:4000".parse().unwrap(),
            infra_listen_addr: None,
            session_secret: "test-session-secret".to_owned(),
            token_pepper: "test-token-pepper".to_owned(),
            vapid_private_key: None,
            object_storage_backend: ObjectStorageBackend::Local,
            media_root: "./media".to_owned(),
            registration_mode: RegistrationMode::Closed,
            registration_rate_limit: RegistrationRateLimitConfig::default(),
            search_indexing_enabled: true.into(),
            federation_enabled: true,
            federation_key_encryption_secret: Some("test-federation-secret".to_owned()),
            federation_allowed_domains: vec!["*".to_owned()],
            federation_delivery_max_age: time::Duration::days(7),
            federation_key_rotation_interval: time::Duration::days(90),
            federation_key_overlap: time::Duration::days(7),
            remote_media_cache_ttl: time::Duration::days(30),
            remote_media_max_bytes: 40 * 1024 * 1024,
            remote_media_fetch_concurrency: 5,
            preview_card_fetch_concurrency: 5,
            worker_concurrency: 4,
            successful_job_retention: time::Duration::hours(24),
            permanently_failed_job_retention: time::Duration::days(30),
            trends_refresh_interval: time::Duration::minutes(5),
            account_suggestions_refresh_interval: time::Duration::hours(24),
            scheduled_statuses: ScheduledStatusConfig::default(),
            streaming: StreamingConfig::default(),
            instance_name: "Roosty Test".to_owned(),
            instance_description: None,
        };

        assert!(config.federation_domain_is_allowed("remote.example"));
        assert!(config.federation_domain_is_allowed("REMOTE.EXAMPLE"));
        assert!(config.federation_domain_is_allowed("blocked.example"));
        assert!(config.federation_domain_is_allowed("notblocked.example"));
    }

    #[test]
    fn resolves_zero_worker_concurrency_to_available_cpus() {
        assert_eq!(resolve_worker_concurrency(3).unwrap(), 3);
        assert_eq!(
            resolve_worker_concurrency(0).unwrap(),
            thread::available_parallelism().unwrap().get()
        );
    }

    #[test]
    fn nonzero_durations_accept_humantime_forms() {
        assert_eq!(
            nonzero_duration("ROOSTY_STREAMING_SEND_TIMEOUT", "1500ms").unwrap(),
            Duration::from_millis(1_500)
        );
        assert_eq!(
            nonzero_duration("ROOSTY_STREAMING_EVENT_RETENTION", "1h").unwrap(),
            Duration::from_secs(3_600)
        );
    }

    #[test]
    fn nonzero_durations_name_invalid_configuration() {
        for value in ["0s", "tomorrow"] {
            let error = nonzero_duration("ROOSTY_STREAMING_IDLE_TIMEOUT", value).unwrap_err();
            let message = error.to_string();
            assert!(message.contains("ROOSTY_STREAMING_IDLE_TIMEOUT"));
            assert!(message.contains("10s"));
        }
    }

    #[test]
    fn trend_refresh_interval_defaults_and_minimum_are_validated() {
        assert_eq!(
            validate_trends_refresh_interval(time::Duration::minutes(5)).unwrap(),
            time::Duration::minutes(5)
        );
        assert_eq!(
            validate_trends_refresh_interval(time::Duration::minutes(17)).unwrap(),
            time::Duration::minutes(17)
        );
        for duration in [time::Duration::ZERO, time::Duration::seconds(59)] {
            let error = validate_trends_refresh_interval(duration).unwrap_err();
            assert!(error.to_string().contains("ROOSTY_TRENDS_REFRESH_INTERVAL"));
        }
    }

    #[test]
    fn account_suggestion_refresh_interval_defaults_to_one_day_and_must_be_nonzero() {
        assert_eq!(
            nonzero_duration(
                "ROOSTY_ACCOUNT_SUGGESTIONS_REFRESH_INTERVAL",
                DEFAULT_ACCOUNT_SUGGESTIONS_REFRESH_INTERVAL,
            )
            .unwrap(),
            Duration::from_secs(24 * 60 * 60)
        );
        for value in ["0s", "daily"] {
            let error =
                nonzero_duration("ROOSTY_ACCOUNT_SUGGESTIONS_REFRESH_INTERVAL", value).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("ROOSTY_ACCOUNT_SUGGESTIONS_REFRESH_INTERVAL")
            );
        }
    }

    #[test]
    fn job_retention_defaults_are_positive_humantime_durations() {
        assert_eq!(
            nonzero_duration(
                "ROOSTY_SUCCESSFUL_JOB_RETENTION",
                DEFAULT_SUCCESSFUL_JOB_RETENTION,
            )
            .unwrap(),
            Duration::from_secs(24 * 60 * 60)
        );
        assert_eq!(
            nonzero_duration(
                "ROOSTY_PERMANENTLY_FAILED_JOB_RETENTION",
                DEFAULT_PERMANENTLY_FAILED_JOB_RETENTION,
            )
            .unwrap(),
            Duration::from_secs(30 * 24 * 60 * 60)
        );
        for name in [
            "ROOSTY_SUCCESSFUL_JOB_RETENTION",
            "ROOSTY_PERMANENTLY_FAILED_JOB_RETENTION",
        ] {
            let error = nonzero_duration(name, "0s").unwrap_err();
            assert!(error.to_string().contains(name));
        }
    }

    #[test]
    fn preview_fetch_concurrency_rejects_zero() {
        assert_eq!(
            positive_concurrency("ROOSTY_PREVIEW_CARD_FETCH_CONCURRENCY", 5).unwrap(),
            5
        );
        assert!(
            positive_concurrency("ROOSTY_PREVIEW_CARD_FETCH_CONCURRENCY", 0)
                .unwrap_err()
                .to_string()
                .contains("ROOSTY_PREVIEW_CARD_FETCH_CONCURRENCY")
        );
    }

    #[test]
    fn streaming_defaults_match_documented_operational_values() {
        assert_eq!(
            StreamingConfig::default(),
            StreamingConfig {
                max_connections: 1_000,
                send_timeout: Duration::from_secs(10),
                ping_interval: Duration::from_secs(30),
                idle_timeout: Duration::from_secs(90),
                event_retention: Duration::from_secs(3_600),
            }
        );
    }

    #[test]
    fn scheduled_status_defaults_match_mastodon() {
        assert_eq!(
            ScheduledStatusConfig::default(),
            ScheduledStatusConfig {
                minimum_offset: time::Duration::minutes(5),
                total_limit: 300,
                daily_limit: 25,
            }
        );
    }

    #[test]
    fn streaming_validation_rejects_zero_connections_and_short_idle_timeout() {
        assert!(
            StreamingConfig::validated(
                0,
                Duration::from_secs(10),
                Duration::from_secs(30),
                Duration::from_secs(90),
                Duration::from_secs(3_600),
            )
            .unwrap_err()
            .to_string()
            .contains("ROOSTY_STREAMING_MAX_CONNECTIONS")
        );
        for zero_duration_index in 0..4 {
            let mut durations = [
                Duration::from_secs(10),
                Duration::from_secs(30),
                Duration::from_secs(90),
                Duration::from_secs(3_600),
            ];
            durations[zero_duration_index] = Duration::ZERO;
            assert!(
                StreamingConfig::validated(
                    1,
                    durations[0],
                    durations[1],
                    durations[2],
                    durations[3],
                )
                .is_err()
            );
        }
        for idle_seconds in [29, 30] {
            assert!(
                StreamingConfig::validated(
                    1,
                    Duration::from_secs(10),
                    Duration::from_secs(30),
                    Duration::from_secs(idle_seconds),
                    Duration::from_secs(3_600),
                )
                .unwrap_err()
                .to_string()
                .contains("ROOSTY_STREAMING_IDLE_TIMEOUT")
            );
        }
    }
}
