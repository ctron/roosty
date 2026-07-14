#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub type Result<T, E = RoostyError> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum RoostyError {
    #[error("configuration error: {0}")]
    Configuration(String),

    #[error("database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("invalid input: {0}")]
    InvalidInput(String),

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
