use std::net::SocketAddr;

use roost_core::{Result, RoostError};
use url::Url;

const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:4000";
const DEFAULT_MEDIA_ROOT: &str = "./media";
const DEFAULT_OBJECT_STORAGE_BACKEND: &str = "local";
const DEFAULT_REGISTRATION_MODE: &str = "closed";

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
    pub instance_name: String,
    pub instance_description: Option<String>,
}

impl Config {
    pub fn from_env(listen_override: Option<SocketAddr>) -> Result<Self> {
        let listen_addr = match listen_override {
            Some(listen) => listen,
            None => parse_env("ROOST_LISTEN_ADDR", DEFAULT_LISTEN_ADDR)?,
        };

        Ok(Self {
            database_url: required_env("ROOST_DATABASE_URL")?,
            public_base_url: required_env("ROOST_PUBLIC_BASE_URL")?
                .parse()
                .map_err(|error| {
                    RoostError::Configuration(format!("ROOST_PUBLIC_BASE_URL is invalid: {error}"))
                })?,
            listen_addr,
            infra_listen_addr: optional_parse_env("ROOST_INFRA_LISTEN_ADDR")?,
            session_secret: required_secret("ROOST_SESSION_SECRET")?,
            token_pepper: required_secret("ROOST_TOKEN_PEPPER")?,
            object_storage_backend: optional_env("ROOST_OBJECT_STORAGE_BACKEND")
                .unwrap_or_else(|| DEFAULT_OBJECT_STORAGE_BACKEND.to_owned()),
            media_root: optional_env("ROOST_MEDIA_ROOT")
                .unwrap_or_else(|| DEFAULT_MEDIA_ROOT.to_owned()),
            registration_mode: optional_env("ROOST_REGISTRATION_MODE")
                .unwrap_or_else(|| DEFAULT_REGISTRATION_MODE.to_owned()),
            federation_enabled: optional_bool_env("ROOST_FEDERATION_ENABLED")?.unwrap_or(false),
            instance_name: required_env("ROOST_INSTANCE_NAME")?,
            instance_description: optional_env("ROOST_INSTANCE_DESCRIPTION"),
        })
    }
}

pub fn database_url_from_env() -> Result<String> {
    required_env("ROOST_DATABASE_URL")
}

fn required_env(name: &str) -> Result<String> {
    let value = std::env::var(name)
        .map_err(|_| RoostError::Configuration(format!("{name} is required")))?;
    if value.trim().is_empty() {
        return Err(RoostError::Configuration(format!(
            "{name} must not be empty"
        )));
    }

    Ok(value)
}

fn required_secret(name: &str) -> Result<String> {
    let value = required_env(name)?;
    if value.len() < 32 {
        return Err(RoostError::Configuration(format!(
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
        .map_err(|error| RoostError::Configuration(format!("{name} is invalid: {error}")))
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
                .map_err(|error| RoostError::Configuration(format!("{name} is invalid: {error}")))
        })
        .transpose()
}

fn optional_bool_env(name: &str) -> Result<Option<bool>> {
    optional_env(name)
        .map(|value| match value.as_str() {
            "1" | "true" | "TRUE" | "yes" | "YES" => Ok(true),
            "0" | "false" | "FALSE" | "no" | "NO" => Ok(false),
            _ => Err(RoostError::Configuration(format!(
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

    fn optional_bool_value(value: &str) -> Result<bool> {
        match value {
            "1" | "true" | "TRUE" | "yes" | "YES" => Ok(true),
            "0" | "false" | "FALSE" | "no" | "NO" => Ok(false),
            _ => Err(RoostError::Configuration(
                "ROOST_FEDERATION_ENABLED must be a boolean".to_owned(),
            )),
        }
    }
}
