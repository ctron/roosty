//! Public build identity for operators and bug reports.

use axum::{Json, Router, routing::get};
use build_info::{BuildInfo, VersionControl};
use serde::Serialize;

use crate::http::AppState;

build_info::build_info!(fn build_info);

/// Return the Git identity most useful to people running this build.
///
/// A release tag is preferred when the current commit has one; development
/// builds fall back to the abbreviated commit hash.
pub(crate) fn build_identifier() -> String {
    build_identifier_from(build_info())
}

fn build_identifier_from(info: &BuildInfo) -> String {
    let Some(version_control) = info.version_control.as_ref() else {
        return info.crate_info.version.to_string();
    };
    let VersionControl::Git(git) = version_control;
    let identifier = git
        .tags
        .first()
        .cloned()
        .unwrap_or_else(|| git.commit_short_id.clone());
    if git.dirty {
        format!("{identifier}-dirty")
    } else {
        identifier
    }
}

/// Build the public route that identifies the running Roosty binary.
pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/version", get(version))
}

async fn version() -> Json<VersionResponse<'static>> {
    Json(VersionResponse::from(build_info()))
}

/// Stable, compact build identity exposed without authentication.
#[derive(Debug, Serialize)]
struct VersionResponse<'a> {
    name: &'a str,
    version: String,
    git: Option<GitVersionResponse<'a>>,
    built_at: String,
}

#[derive(Debug, Serialize)]
struct GitVersionResponse<'a> {
    commit: &'a str,
    commit_short: &'a str,
    dirty: bool,
}

impl<'a> From<&'a BuildInfo> for VersionResponse<'a> {
    fn from(info: &'a BuildInfo) -> Self {
        let git = info.version_control.as_ref().map(|version_control| {
            let VersionControl::Git(git) = version_control;
            GitVersionResponse {
                commit: &git.commit_id,
                commit_short: &git.commit_short_id,
                dirty: git.dirty,
            }
        });

        Self {
            name: &info.crate_info.name,
            version: info.crate_info.version.to_string(),
            git,
            built_at: info.timestamp.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    use super::*;

    /// Given the compiled server, the endpoint reports its package and build identity.
    #[tokio::test]
    async fn version_endpoint_reports_build_identity() {
        let response = Router::new()
            .route("/api/v1/version", get(version))
            .oneshot(
                Request::builder()
                    .uri("/api/v1/version")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(response.status().is_success());
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(body["name"], env!("CARGO_PKG_NAME"));
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
        assert!(body["built_at"].as_str().is_some());
        if let Some(git) = build_info().version_control.as_ref() {
            assert_eq!(body["git"]["commit"], git.git().unwrap().commit_id);
        } else {
            assert!(body["git"].is_null());
        }
    }
}
