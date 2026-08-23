//! Search-crawler policy and bounded sitemap discovery endpoints.

use axum::{
    Extension, Router,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::http::{ApiResult, AppState, DatabaseContext};

const PUBLIC_CACHE_CONTROL: &str = "public, max-age=300";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/robots.txt", get(robots))
        .route("/sitemap.xml", get(sitemap_index))
        .route("/sitemaps/profiles/{cursor}", get(profile_sitemap))
        .route("/sitemaps/posts/{cursor}", get(status_sitemap))
}

async fn robots(State(state): State<AppState>) -> Response {
    text_response(robots_body(
        state.config.search_indexing_enabled.is_enabled(),
        &absolute_url(&state, "/sitemap.xml"),
    ))
}

fn robots_body(indexing_enabled: bool, sitemap_url: &str) -> String {
    let mut body = "User-agent: *\nAllow: /\n".to_owned();
    if indexing_enabled {
        body.push_str(&format!("Sitemap: {sitemap_url}\n"));
    }
    body
}

async fn sitemap_index(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
) -> ApiResult<Response> {
    if !state.config.search_indexing_enabled.is_enabled() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let txn = database.begin_read().await?;
    let (profiles, statuses) = tokio::try_join!(
        roosty_db::search_profile_sitemap_chunks(&txn),
        roosty_db::search_status_sitemap_chunks(&txn),
    )?;
    txn.commit().await?;
    let mut body = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<sitemapindex xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
    );
    for (kind, chunk) in profiles
        .iter()
        .map(|chunk| ("profiles", chunk))
        .chain(statuses.iter().map(|chunk| ("posts", chunk)))
    {
        let location = absolute_url(
            &state,
            &format!("/sitemaps/{kind}/{}.xml", encode_cursor(chunk.cursor)),
        );
        push_sitemap(&mut body, &location, chunk.last_modified);
    }
    body.push_str("</sitemapindex>\n");
    Ok(xml_response(body))
}

async fn profile_sitemap(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    Path(cursor): Path<String>,
) -> ApiResult<Response> {
    if !state.config.search_indexing_enabled.is_enabled() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let Some(cursor) = decode_cursor(&cursor) else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    let txn = database.begin_read().await?;
    let urls = roosty_db::search_profile_sitemap_urls(&txn, cursor).await?;
    txn.commit().await?;
    let mut body = urlset_start();
    for url in urls {
        let location = absolute_url(&state, &format!("/@{}", url.username));
        push_url(&mut body, &location, url.last_modified);
    }
    body.push_str("</urlset>\n");
    Ok(xml_response(body))
}

async fn status_sitemap(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    Path(cursor): Path<String>,
) -> ApiResult<Response> {
    if !state.config.search_indexing_enabled.is_enabled() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let Some(cursor) = decode_cursor(&cursor) else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    let txn = database.begin_read().await?;
    let urls = roosty_db::search_status_sitemap_urls(&txn, cursor).await?;
    txn.commit().await?;
    let mut body = urlset_start();
    for url in urls {
        let location = absolute_url(&state, &format!("/@{}/{}", url.username, url.id));
        push_url(&mut body, &location, url.last_modified);
    }
    body.push_str("</urlset>\n");
    Ok(xml_response(body))
}

fn encode_cursor(cursor: Uuid) -> String {
    URL_SAFE_NO_PAD.encode(cursor.as_bytes())
}

fn decode_cursor(value: &str) -> Option<Uuid> {
    let value = value.strip_suffix(".xml")?;
    let bytes = URL_SAFE_NO_PAD.decode(value).ok()?;
    Uuid::from_slice(&bytes).ok()
}

fn absolute_url(state: &AppState, path: &str) -> String {
    format!(
        "{}{}",
        state.config.public_base_url.as_str().trim_end_matches('/'),
        path
    )
}

fn urlset_start() -> String {
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n".to_owned()
}

fn push_sitemap(body: &mut String, location: &str, last_modified: OffsetDateTime) {
    body.push_str("  <sitemap><loc>");
    body.push_str(&xml_escape(location));
    body.push_str("</loc><lastmod>");
    body.push_str(&format_last_modified(last_modified));
    body.push_str("</lastmod></sitemap>\n");
}

fn push_url(body: &mut String, location: &str, last_modified: OffsetDateTime) {
    body.push_str("  <url><loc>");
    body.push_str(&xml_escape(location));
    body.push_str("</loc><lastmod>");
    body.push_str(&format_last_modified(last_modified));
    body.push_str("</lastmod></url>\n");
}

fn format_last_modified(value: OffsetDateTime) -> String {
    value.format(&Rfc3339).unwrap_or_default()
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn xml_response(body: String) -> Response {
    cached_response("application/xml; charset=utf-8", body)
}

fn text_response(body: String) -> Response {
    cached_response("text/plain; charset=utf-8", body)
}

fn cached_response(content_type: &'static str, body: String) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(PUBLIC_CACHE_CONTROL),
    );
    (headers, body).into_response()
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;
    use uuid::Uuid;

    use super::{decode_cursor, encode_cursor, push_url, robots_body, urlset_start, xml_escape};

    #[test]
    fn robots_keeps_crawling_allowed_and_only_advertises_enabled_sitemap() {
        let sitemap = "https://roosty.test/sitemap.xml";
        assert!(robots_body(true, sitemap).contains(&format!("Sitemap: {sitemap}")));
        let disabled = robots_body(false, sitemap);
        assert!(disabled.contains("Allow: /"));
        assert!(!disabled.contains("Sitemap:"));
    }

    #[test]
    fn cursor_is_opaque_url_safe_and_round_trips() {
        let id = Uuid::parse_str("0198a31c-2c00-7000-8000-000000000001").unwrap();
        let encoded = encode_cursor(id);
        assert!(!encoded.contains(['/', '+', '=']));
        assert_eq!(decode_cursor(&format!("{encoded}.xml")), Some(id));
        assert_eq!(decode_cursor(&encoded), None);
    }

    #[test]
    fn sitemap_values_are_xml_escaped_and_include_absolute_lastmod() {
        assert_eq!(
            xml_escape("https://x.test/?a=1&b=<x>"),
            "https://x.test/?a=1&amp;b=&lt;x&gt;"
        );
        let mut body = urlset_start();
        push_url(
            &mut body,
            "https://x.test/@a?x=1&y=2",
            datetime!(2026-08-23 12:00 UTC),
        );
        assert!(body.contains("https://x.test/@a?x=1&amp;y=2"));
        assert!(body.contains("2026-08-23T12:00:00Z"));
    }
}
