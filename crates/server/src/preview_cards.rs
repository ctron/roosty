//! Durable, bounded fetching of Mastodon-compatible link preview metadata.

use std::{
    borrow::Cow,
    io::Error as IoError,
    net::{IpAddr, SocketAddr},
    string::FromUtf8Error,
    time::Duration,
};

use reqwest::{
    Client, Response,
    header::{CONTENT_TYPE, LOCATION, ToStrError},
    redirect::Policy,
};
use roosty_core::{Result as RoostyResult, RoostyError};
use roosty_db::PreviewCardUpdate;
use scraper::{Html, Selector};
use sea_orm::DbErr;
use serde_json::Value;
use thiserror::Error;
use tokio::{net::lookup_host, sync::AcquireError};
use url::{ParseError as UrlParseError, Url};
use uuid::{Error, Uuid};

use crate::{
    federation::discovery::is_unsafe_address,
    http::{AppState, DatabaseContext},
    media::store_preview_card_image,
};

const MAX_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

type PreviewResult<T> = Result<T, PreviewCardError>;

#[derive(Debug, Error)]
enum PreviewCardError {
    #[error(transparent)]
    Core(#[from] RoostyError),
    #[error(transparent)]
    Database(#[from] DbErr),
    #[error("preview fetch pool is closed")]
    Permit(#[from] AcquireError),
    #[error("preview card id is invalid")]
    Identifier(#[from] Error),
    #[error("preview URL is invalid: {0}")]
    Url(#[from] UrlParseError),
    #[error("preview document is not UTF-8: {0}")]
    Utf8(#[from] FromUtf8Error),
    #[error("preview request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("preview response header is invalid: {0}")]
    Header(#[from] ToStrError),
    #[error("preview network lookup failed: {0}")]
    Io(#[from] IoError),
    #[error("{0}")]
    Invalid(Cow<'static, str>),
}

impl From<PreviewCardError> for RoostyError {
    fn from(error: PreviewCardError) -> Self {
        match error {
            PreviewCardError::Core(error) => error,
            PreviewCardError::Database(error) => error.into(),
            PreviewCardError::Permit(_) => Self::Configuration(error.to_string()),
            error => Self::InvalidInput(error.to_string()),
        }
    }
}

enum FetchOutcome {
    Redirect(Url),
    Document {
        content_type: String,
        bytes: Vec<u8>,
    },
}

/// Fetch and persist one preview card named by a durable job payload.
pub(crate) async fn fetch_preview_card(
    state: &AppState,
    database: &DatabaseContext,
    payload: Value,
    attempts: u32,
) -> RoostyResult<()> {
    fetch_preview_card_with_context(state, database, payload, attempts).await?;
    Ok(())
}

async fn fetch_preview_card_with_context(
    state: &AppState,
    database: &DatabaseContext,
    payload: Value,
    attempts: u32,
) -> PreviewResult<()> {
    let _permit = state.preview_card_fetches.acquire().await?;
    let id = payload
        .get("preview_card_id")
        .and_then(Value::as_str)
        .ok_or(PreviewCardError::Invalid(
            "preview card id is missing".into(),
        ))?
        .parse::<Uuid>()?;
    let txn = database.begin_read().await?;
    let card = roosty_db::preview_card_by_id(&txn, id).await?;
    txn.commit().await?;
    let Some(card) = card else {
        return Ok(());
    };
    let url = Url::parse(&card.url)?;
    let update = if attempts >= 4 {
        fetch_preview_card_inner(state, database, id, url)
            .await
            .inspect_err(|error| {
                tracing::warn!(%id, %error, "preview card fetch exhausted retries");
            })
            .ok()
    } else {
        Some(fetch_preview_card_inner(state, database, id, url).await?)
    };
    let txn = database.begin_write().await?;
    if let Some(update) = update {
        roosty_db::update_preview_card(&txn, id, update).await?;
    } else {
        roosty_db::mark_preview_card_failed(&txn, id).await?;
    }
    txn.commit().await?;
    Ok(())
}

async fn fetch_preview_card_inner(
    state: &AppState,
    database: &DatabaseContext,
    id: Uuid,
    url: Url,
) -> PreviewResult<PreviewCardUpdate> {
    let (final_url, _content_type, bytes) =
        fetch_bytes(state, database, url, MAX_DOCUMENT_BYTES, Some("text/html")).await?;
    let content = String::from_utf8(bytes)?;
    let metadata = parse_metadata(&content, &final_url);
    let (image_file_path, image_width, image_height, blurhash) = if let Some(image_url) =
        metadata.image_url
    {
        let image = fetch_bytes(
            state,
            database,
            image_url,
            usize::try_from(state.config.remote_media_max_bytes).unwrap_or(usize::MAX),
            Some("image/"),
        )
        .await
        .inspect_err(|error| tracing::warn!(%id, %error, "preview image fetch failed"))
        .ok();
        let stored = if let Some((_, image_type, image_bytes)) = image {
            store_preview_card_image(state, id, image_bytes, &image_type)
                .await
                .inspect_err(|error| tracing::warn!(%id, %error, "preview image was not cacheable"))
                .ok()
        } else {
            None
        };
        if let Some((path, width, height, blurhash)) = stored {
            (Some(path), width, height, Some(blurhash))
        } else {
            (None, 0, 0, None)
        }
    } else {
        (None, 0, 0, None)
    };
    let provider_name = metadata.provider_name.unwrap_or_else(|| {
        final_url
            .host_str()
            .unwrap_or_default()
            .trim_start_matches("www.")
            .to_owned()
    });
    Ok(PreviewCardUpdate {
        title: metadata.title,
        description: metadata.description,
        author_name: metadata.author_name,
        author_url: metadata.author_url,
        provider_name,
        provider_url: final_url.origin().ascii_serialization(),
        image_file_path,
        image_width,
        image_height,
        blurhash,
        published_at: None,
    })
}

async fn fetch_bytes(
    state: &AppState,
    database: &DatabaseContext,
    mut url: Url,
    maximum: usize,
    expected_type: Option<&str>,
) -> PreviewResult<(Url, String, Vec<u8>)> {
    for _ in 0..=MAX_REDIRECTS {
        let host = url
            .host_str()
            .ok_or(PreviewCardError::Invalid("preview URL has no host".into()))?
            .to_ascii_lowercase();
        let txn = database.begin_read().await?;
        let domain_policy = roosty_db::federation_domain_policy(&txn, &host).await?;
        txn.commit().await?;
        if !state.config.federation_domain_is_allowed(&host)
            || domain_policy.is_suspended()
            || domain_policy.reject_media
            || host.parse::<IpAddr>().is_ok()
            || !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(PreviewCardError::Invalid(
                "preview URL is not permitted".into(),
            ));
        }
        let port = url
            .port_or_known_default()
            .ok_or(PreviewCardError::Invalid("preview URL has no port".into()))?;
        let addresses = lookup_host((host.as_str(), port))
            .await?
            .collect::<Vec<_>>();
        if addresses.is_empty()
            || addresses
                .iter()
                .any(|address| is_unsafe_address(address.ip()))
        {
            return Err(PreviewCardError::Invalid(
                "preview host resolves to an unsafe address".into(),
            ));
        }
        let address = addresses[0];
        let txn = database.begin_write().await?;
        let acquired = roosty_db::acquire_preview_host_lease(&txn, &host).await?;
        txn.commit().await?;
        if !acquired {
            return Err(PreviewCardError::Invalid(
                "preview host is currently rate limited".into(),
            ));
        }
        let result = fetch_once(&url, &host, address, maximum, expected_type).await;
        let txn = database.begin_write().await?;
        roosty_db::release_preview_host_lease(&txn, &host).await?;
        txn.commit().await?;
        let outcome = result?;
        match outcome {
            FetchOutcome::Redirect(next) => url = next,
            FetchOutcome::Document {
                content_type,
                bytes,
            } => return Ok((url, content_type, bytes)),
        }
    }
    Err(PreviewCardError::Invalid(
        "preview response has too many redirects".into(),
    ))
}

async fn fetch_once(
    url: &Url,
    host: &str,
    address: SocketAddr,
    maximum: usize,
    expected_type: Option<&str>,
) -> PreviewResult<FetchOutcome> {
    let client = Client::builder()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .resolve(host, address)
        .build()?;
    let response = client.get(url.clone()).send().await?;
    if response.status().is_redirection() {
        let location = response
            .headers()
            .get(LOCATION)
            .ok_or(PreviewCardError::Invalid(
                "preview redirect has no location".into(),
            ))?
            .to_str()?;
        return Ok(FetchOutcome::Redirect(url.join(location)?));
    }
    validate_response(&response, maximum, expected_type)?;
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .map(|value| value.to_str())
        .transpose()?
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let bytes = response.bytes().await?.to_vec();
    if bytes.len() > maximum {
        return Err(PreviewCardError::Invalid(
            "preview response exceeds the size limit".into(),
        ));
    }
    Ok(FetchOutcome::Document {
        content_type,
        bytes,
    })
}

fn validate_response(
    response: &Response,
    maximum: usize,
    expected_type: Option<&str>,
) -> PreviewResult<()> {
    if !response.status().is_success() {
        return Err(PreviewCardError::Invalid(Cow::Owned(format!(
            "preview server returned {}",
            response.status()
        ))));
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .map(|value| value.to_str())
        .transpose()?
        .unwrap_or_default();
    if expected_type.is_some_and(|expected| !content_type.starts_with(expected)) {
        return Err(PreviewCardError::Invalid(
            "preview response has an unsupported content type".into(),
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(PreviewCardError::Invalid(
            "preview response exceeds the size limit".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Default, Eq, PartialEq)]
struct ParsedMetadata {
    title: String,
    description: String,
    author_name: String,
    author_url: String,
    provider_name: Option<String>,
    image_url: Option<Url>,
}

fn parse_metadata(content: &str, base: &Url) -> ParsedMetadata {
    let document = Html::parse_document(content);
    let title = meta(&document, "property", "og:title")
        .or_else(|| text(&document, "title"))
        .unwrap_or_default();
    let description = meta(&document, "property", "og:description")
        .or_else(|| meta(&document, "name", "description"))
        .unwrap_or_default();
    let author_name = meta(&document, "name", "author").unwrap_or_default();
    let author_url = meta(&document, "property", "article:author").unwrap_or_default();
    let provider_name = meta(&document, "property", "og:site_name");
    let image_url = meta(&document, "property", "og:image")
        .and_then(|value| base.join(&value).ok())
        .filter(|url| matches!(url.scheme(), "http" | "https"));
    ParsedMetadata {
        title,
        description,
        author_name,
        author_url,
        provider_name,
        image_url,
    }
}

fn meta(document: &Html, attribute: &str, value: &str) -> Option<String> {
    let selector = Selector::parse(&format!("meta[{attribute}=\"{value}\"]")).ok()?;
    document
        .select(&selector)
        .next()
        .and_then(|element| element.value().attr("content"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn text(document: &Html, selector: &str) -> Option<String> {
    let selector = Selector::parse(selector).ok()?;
    let value = document
        .select(&selector)
        .next()?
        .text()
        .collect::<String>();
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_open_graph_with_standard_fallbacks() {
        let base = Url::parse("https://news.example/articles/one").ok();
        let Some(base) = base else {
            return;
        };
        let parsed = parse_metadata(
            r#"<html><head>
              <meta property="og:title" content="A story">
              <meta name="description" content="The summary">
              <meta property="og:image" content="/image.png">
              <meta property="og:site_name" content="Example News">
              <meta name="author" content="Alice">
            </head></html>"#,
            &base,
        );
        assert_eq!(parsed.title, "A story");
        assert_eq!(parsed.description, "The summary");
        assert_eq!(parsed.author_name, "Alice");
        assert_eq!(parsed.provider_name.as_deref(), Some("Example News"));
        assert_eq!(
            parsed.image_url.as_ref().map(Url::as_str),
            Some("https://news.example/image.png")
        );
    }
}
