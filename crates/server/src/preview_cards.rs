//! Durable, bounded fetching of Mastodon-compatible link preview metadata.

use std::{net::IpAddr, time::Duration};

use reqwest::header::CONTENT_TYPE;
use roosty_core::{Result, RoostyError};
use roosty_db::PreviewCardUpdate;
use scraper::{Html, Selector};
use serde_json::Value;
use url::Url;
use uuid::Uuid;

use crate::{
    federation::discovery::is_unsafe_address, http::AppState, media::store_preview_card_image,
};

const MAX_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

/// Fetch and persist one preview card named by a durable job payload.
pub(crate) async fn fetch_preview_card(
    state: &AppState,
    payload: Value,
    attempts: u32,
) -> Result<()> {
    let _permit = state
        .preview_card_fetches
        .acquire()
        .await
        .map_err(|_| RoostyError::Configuration("preview fetch pool is closed".to_owned()))?;
    let id = payload
        .get("preview_card_id")
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or_else(|| RoostyError::InvalidInput("preview card id is invalid".to_owned()))?;
    let Some(card) = roosty_db::preview_card_by_id(&state.db, id).await? else {
        return Ok(());
    };
    let url = Url::parse(&card.url)
        .map_err(|_| RoostyError::InvalidInput("preview card URL is invalid".to_owned()))?;
    let result = fetch_preview_card_inner(state, id, url).await;
    match result {
        Ok(update) => roosty_db::update_preview_card(&state.db, id, update).await,
        Err(error) if attempts >= 4 => {
            roosty_db::mark_preview_card_failed(&state.db, id).await?;
            tracing::warn!(%id, %error, "preview card fetch exhausted retries");
            Ok(())
        }
        Err(error) => Err(error),
    }
}

async fn fetch_preview_card_inner(
    state: &AppState,
    id: Uuid,
    url: Url,
) -> Result<PreviewCardUpdate> {
    let (final_url, _content_type, bytes) =
        fetch_bytes(state, url, MAX_DOCUMENT_BYTES, Some("text/html")).await?;
    let content = String::from_utf8(bytes)
        .map_err(|_| RoostyError::InvalidInput("preview document is not UTF-8".to_owned()))?;
    let metadata = parse_metadata(&content, &final_url);
    let (image_file_path, image_width, image_height, blurhash) =
        if let Some(image_url) = metadata.image_url {
            let image = fetch_bytes(
                state,
                image_url,
                usize::try_from(state.config.remote_media_max_bytes).unwrap_or(usize::MAX),
                Some("image/"),
            )
            .await;
            match image {
                Ok((_, image_type, image_bytes)) => {
                    match store_preview_card_image(state, id, image_bytes, &image_type).await {
                        Ok((path, width, height, blurhash)) => {
                            (Some(path), width, height, Some(blurhash))
                        }
                        Err(error) => {
                            tracing::warn!(%id, %error, "preview image was not cacheable");
                            (None, 0, 0, None)
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(%id, %error, "preview image fetch failed");
                    (None, 0, 0, None)
                }
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
    mut url: Url,
    maximum: usize,
    expected_type: Option<&str>,
) -> Result<(Url, String, Vec<u8>)> {
    for _ in 0..=MAX_REDIRECTS {
        let host = url
            .host_str()
            .ok_or_else(|| RoostyError::InvalidInput("preview URL has no host".to_owned()))?
            .to_ascii_lowercase();
        if !state.config.federation_domain_is_allowed(&host)
            || host.parse::<IpAddr>().is_ok()
            || !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(RoostyError::InvalidInput(
                "preview URL is not permitted".to_owned(),
            ));
        }
        let port = url
            .port_or_known_default()
            .ok_or_else(|| RoostyError::InvalidInput("preview URL has no port".to_owned()))?;
        let addresses = tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|_| RoostyError::InvalidInput("preview host did not resolve".to_owned()))?
            .collect::<Vec<_>>();
        if addresses.is_empty()
            || addresses
                .iter()
                .any(|address| is_unsafe_address(address.ip()))
        {
            return Err(RoostyError::InvalidInput(
                "preview host resolves to an unsafe address".to_owned(),
            ));
        }
        let address = addresses[0];
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .resolve(&host, address)
            .build()
            .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
        if !roosty_db::acquire_preview_host_lease(&state.db, &host).await? {
            return Err(RoostyError::InvalidInput(
                "preview host is currently rate limited".to_owned(),
            ));
        }
        let response = client.get(url.clone()).send().await;
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                roosty_db::release_preview_host_lease(&state.db, &host).await?;
                return Err(RoostyError::InvalidInput(error.to_string()));
            }
        };
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| {
                    RoostyError::InvalidInput("preview redirect has no location".to_owned())
                })?;
            url = url
                .join(location)
                .map_err(|_| RoostyError::InvalidInput("preview redirect is invalid".to_owned()))?;
            roosty_db::release_preview_host_lease(&state.db, &host).await?;
            continue;
        }
        if !response.status().is_success() {
            roosty_db::release_preview_host_lease(&state.db, &host).await?;
            return Err(RoostyError::InvalidInput(format!(
                "preview server returned {}",
                response.status()
            )));
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .split(';')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if expected_type.is_some_and(|expected| !content_type.starts_with(expected)) {
            roosty_db::release_preview_host_lease(&state.db, &host).await?;
            return Err(RoostyError::InvalidInput(
                "preview response has an unsupported content type".to_owned(),
            ));
        }
        if response
            .content_length()
            .is_some_and(|length| length > maximum as u64)
        {
            roosty_db::release_preview_host_lease(&state.db, &host).await?;
            return Err(RoostyError::InvalidInput(
                "preview response exceeds the size limit".to_owned(),
            ));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| RoostyError::InvalidInput(error.to_string()));
        let bytes = match bytes {
            Ok(bytes) => bytes,
            Err(error) => {
                roosty_db::release_preview_host_lease(&state.db, &host).await?;
                return Err(error);
            }
        };
        if bytes.len() > maximum {
            roosty_db::release_preview_host_lease(&state.db, &host).await?;
            return Err(RoostyError::InvalidInput(
                "preview response exceeds the size limit".to_owned(),
            ));
        }
        roosty_db::release_preview_host_lease(&state.db, &host).await?;
        return Ok((url, content_type, bytes.to_vec()));
    }
    Err(RoostyError::InvalidInput(
        "preview response has too many redirects".to_owned(),
    ))
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
