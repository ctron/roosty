#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::borrow::Cow;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub type Result<T, E = RoostyError> = std::result::Result<T, E>;

/// Typed failures produced while applying remote federation discovery policy.
#[derive(Debug, Error)]
pub enum FederationDiscoveryError {
    #[error("federation policy rejects remote domain {0}")]
    PolicyRejected(Cow<'static, str>),
}

#[derive(Debug, Error)]
pub enum RoostyError {
    #[error("configuration error: {0}")]
    Configuration(String),

    #[error("database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error(transparent)]
    FederationDiscovery(#[from] FederationDiscoveryError),

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("invalid status visibility: {0}")]
    StatusVisibility(#[from] strum::ParseError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("operation is not implemented yet: {0}")]
    NotImplemented(&'static str),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct AccountId(pub Uuid);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct StatusId(pub Uuid);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct JobId(pub Uuid);

/// Opaque lease identity for one attempt to process a durable job.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct JobClaimId(pub Uuid);
