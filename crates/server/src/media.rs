use std::{
    io::Cursor,
    path::{Path, PathBuf},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Path as AxumPath, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use axum_params::{Params, UploadFile};
use image::{GenericImageView, ImageFormat, ImageReader};
use roost_core::{AccountId, RoostError};
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use tokio::task;
use uuid::Uuid;

use crate::{
    auth::{AuthenticatedAccount, OptionalAuthenticatedAccount},
    http::AppState,
};

const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
const DESCRIPTION_LIMIT: usize = 1500;
const PREVIEW_BOUNDING_BOX: u32 = 400;

/// Image formats accepted by local media upload and advertised to clients.
pub(crate) const SUPPORTED_IMAGE_FORMATS: &[SupportedImageFormat] = &[
    SupportedImageFormat::new("image/avif", "avif", ImageFormat::Avif),
    SupportedImageFormat::new("image/bmp", "bmp", ImageFormat::Bmp),
    SupportedImageFormat::new("image/vnd-ms.dds", "dds", ImageFormat::Dds),
    SupportedImageFormat::new("image/x-exr", "exr", ImageFormat::OpenExr),
    SupportedImageFormat::new("image/farbfeld", "ff", ImageFormat::Farbfeld),
    SupportedImageFormat::new("image/gif", "gif", ImageFormat::Gif),
    SupportedImageFormat::new("image/vnd.radiance", "hdr", ImageFormat::Hdr),
    SupportedImageFormat::new("image/vnd.microsoft.icon", "ico", ImageFormat::Ico),
    SupportedImageFormat::new("image/jpeg", "jpg", ImageFormat::Jpeg),
    SupportedImageFormat::new("image/png", "png", ImageFormat::Png),
    SupportedImageFormat::new("image/x-portable-anymap", "pnm", ImageFormat::Pnm),
    SupportedImageFormat::new("image/qoi", "qoi", ImageFormat::Qoi),
    SupportedImageFormat::new("image/x-tga", "tga", ImageFormat::Tga),
    SupportedImageFormat::new("image/tiff", "tiff", ImageFormat::Tiff),
    SupportedImageFormat::new("image/webp", "webp", ImageFormat::WebP),
];

/// Server media format metadata shared by validation, descriptors, and serving.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SupportedImageFormat {
    /// Canonical MIME type accepted from multipart uploads.
    pub(crate) content_type: &'static str,
    /// File extension used when storing original uploads.
    extension: &'static str,
    /// Decoder hint passed to the image crate.
    image_format: ImageFormat,
}

impl SupportedImageFormat {
    const fn new(
        content_type: &'static str,
        extension: &'static str,
        image_format: ImageFormat,
    ) -> Self {
        Self {
            content_type,
            extension,
            image_format,
        }
    }
}

/// Return the supported local media upload MIME types advertised to Mastodon clients.
pub(crate) fn supported_image_mime_types() -> Vec<&'static str> {
    SUPPORTED_IMAGE_FORMATS
        .iter()
        .map(|format| format.content_type)
        .collect()
}

/// Build Mastodon-compatible media upload and serving routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/media", post(upload_media))
        .route("/api/v2/media", post(upload_media))
        .route(
            "/api/v1/media/{media_id}",
            get(get_media).put(update_media).delete(delete_media),
        )
        .route(
            "/media_attachments/files/{*path}",
            get(serve_media_attachment),
        )
}

/// Mastodon media upload form fields.
#[derive(Deserialize)]
struct MediaUploadParams {
    #[serde(default, deserialize_with = "deserialize_optional_upload_file")]
    file: Option<UploadFile>,
    #[serde(default, deserialize_with = "deserialize_optional_upload_file")]
    thumbnail: Option<UploadFile>,
    description: Option<String>,
    focus: Option<String>,
}

/// Mastodon media update form fields.
#[derive(Default, Deserialize)]
struct MediaUpdateParams {
    #[serde(default, deserialize_with = "deserialize_optional_upload_file")]
    file: Option<UploadFile>,
    #[serde(default, deserialize_with = "deserialize_optional_upload_file")]
    thumbnail: Option<UploadFile>,
    description: Option<String>,
    focus: Option<String>,
}

/// Deserialize optional upload fields while accepting client-sent null sentinels.
fn deserialize_optional_upload_file<'de, D>(deserializer: D) -> Result<Option<UploadFile>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserializer.deserialize_any(OptionalUploadFileVisitor)
}

struct OptionalUploadFileVisitor;

impl<'de> Visitor<'de> for OptionalUploadFileVisitor {
    type Value = Option<UploadFile>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a multipart upload file or a null-like text field")
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserialize_optional_upload_file(deserializer)
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if nullish_multipart_value(value) {
            Ok(None)
        } else {
            Err(E::custom("expected upload file"))
        }
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.visit_str(value.as_str())
    }

    fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        UploadFile::deserialize(de::value::MapAccessDeserializer::new(map)).map(Some)
    }
}

/// Return whether a multipart text field should be treated like an omitted upload.
fn nullish_multipart_value(value: &str) -> bool {
    matches!(value.trim(), "" | "null" | "undefined")
}

#[derive(Deserialize)]
struct MediaPath {
    media_id: Uuid,
}

#[derive(Deserialize)]
struct MediaFilePath {
    path: String,
}

/// Mastodon MediaAttachment response shape for local media.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct MediaAttachmentResponse {
    id: String,
    #[serde(rename = "type")]
    media_type: &'static str,
    url: String,
    preview_url: String,
    remote_url: Option<String>,
    meta: MediaMeta,
    description: Option<String>,
    blurhash: Option<String>,
}

/// Mastodon media metadata object for local image attachments.
#[derive(Clone, Debug, Default, Serialize)]
struct MediaMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    original: Option<ImageMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    small: Option<ImageMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    focus: Option<FocusMeta>,
}

/// Width, height, string size, and aspect metadata for a rendered image.
#[derive(Clone, Debug, Serialize)]
struct ImageMeta {
    width: i32,
    height: i32,
    size: String,
    aspect: f64,
}

/// Mastodon focal point metadata.
#[derive(Clone, Debug, Serialize)]
struct FocusMeta {
    x: f64,
    y: f64,
}

/// Error response shape used by Mastodon-compatible media endpoints.
#[derive(Serialize)]
struct ApiError {
    error: String,
}

impl ApiError {
    fn new(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
        }
    }
}

/// CPU-bound metadata and preview output produced from uploaded image bytes.
struct ProcessedImage {
    width: i32,
    height: i32,
    preview_bytes: Vec<u8>,
    preview_width: i32,
    preview_height: i32,
    blurhash: String,
}

async fn upload_media(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Params(params, _temp_files): Params<MediaUploadParams>,
) -> Response {
    if let Err(error) = ensure_local_storage(&state) {
        return server_error(error);
    }
    let Some(file) = params.file else {
        return unprocessable("file is required");
    };
    let format = match validate_image_content_type(&file.content_type) {
        Ok(format) => format,
        Err(error) => return unprocessable(&error),
    };
    let description = match normalize_description(params.description) {
        Ok(description) => description,
        Err(error) => return unprocessable(&error),
    };
    let focus = match parse_focus(params.focus.as_deref()) {
        Ok(focus) => focus,
        Err(error) => return unprocessable(&error),
    };

    match store_upload(
        &state,
        account.id,
        file,
        params.thumbnail,
        format,
        description,
        focus,
    )
    .await
    {
        Ok(media) => Json(media_response(&state, &media)).into_response(),
        Err(MediaStoreError::Validation(error)) => unprocessable(&error),
        Err(MediaStoreError::Roost(error)) => server_error(error),
    }
}

async fn get_media(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    AxumPath(path): AxumPath<MediaPath>,
) -> Response {
    match roost_db::find_owned_unattached_media_attachment(&state.db, account.id, path.media_id)
        .await
    {
        Ok(Some(media)) => Json(media_response(&state, &media)).into_response(),
        Ok(None) => not_found(),
        Err(error) => server_error(error),
    }
}

async fn update_media(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    AxumPath(path): AxumPath<MediaPath>,
    Params(params, _temp_files): Params<MediaUpdateParams>,
) -> Response {
    let description = match params.description {
        Some(description) => match normalize_description(Some(description)) {
            Ok(description) => Some(description),
            Err(error) => return unprocessable(&error),
        },
        None => None,
    };
    let focus = match parse_focus(params.focus.as_deref()) {
        Ok(focus) => focus,
        Err(error) => return unprocessable(&error),
    };
    let thumbnail = params.thumbnail.or(params.file);
    let preview = match replacement_preview(&state, path.media_id, thumbnail).await {
        Ok(preview) => preview,
        Err(MediaStoreError::Validation(error)) => return unprocessable(&error),
        Err(MediaStoreError::Roost(error)) => return server_error(error),
    };
    let update = roost_db::LocalMediaAttachmentUpdate {
        description,
        focus,
        preview,
    };

    match roost_db::update_owned_unattached_media_attachment(
        &state.db,
        account.id,
        path.media_id,
        update,
    )
    .await
    {
        Ok(Some(media)) => Json(media_response(&state, &media)).into_response(),
        Ok(None) => not_found(),
        Err(error) => server_error(error),
    }
}

async fn delete_media(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    AxumPath(path): AxumPath<MediaPath>,
) -> Response {
    match roost_db::delete_owned_unattached_media_attachment(&state.db, account.id, path.media_id)
        .await
    {
        Ok(Some(media)) => {
            let _ = tokio::fs::remove_file(media_path(&state, &media.file_path)).await;
            if let Some(preview_path) = media.preview_file_path {
                let _ = tokio::fs::remove_file(media_path(&state, &preview_path)).await;
            }
            StatusCode::OK.into_response()
        }
        Ok(None) => not_found(),
        Err(error) => server_error(error),
    }
}

async fn serve_media_attachment(
    State(state): State<AppState>,
    OptionalAuthenticatedAccount(_viewer): OptionalAuthenticatedAccount,
    AxumPath(path): AxumPath<MediaFilePath>,
) -> Response {
    let Some(relative_path) = safe_relative_path(&path.path) else {
        return not_found();
    };
    let full_path = Path::new(&state.config.media_root).join(relative_path);
    let content_type = content_type_from_path(&full_path);
    match tokio::fs::read(full_path).await {
        Ok(bytes) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, content_type)],
            Body::from(bytes),
        )
            .into_response(),
        Err(_) => not_found(),
    }
}

/// Build a local media response from stored metadata.
pub(crate) fn media_response(
    state: &AppState,
    media: &roost_db::LocalMediaAttachment,
) -> MediaAttachmentResponse {
    let url = media_url(state, &media.file_path);
    let preview_url = media
        .preview_file_path
        .as_deref()
        .map(|path| media_url(state, path))
        .unwrap_or_else(|| url.clone());
    MediaAttachmentResponse {
        id: media.id.to_string(),
        media_type: media_type(&media.content_type),
        url,
        preview_url,
        remote_url: None,
        meta: media_meta(media),
        description: media.description.clone(),
        blurhash: media.blurhash.clone(),
    }
}

/// Store an original upload, generated preview, and database media metadata.
async fn store_upload(
    state: &AppState,
    account_id: AccountId,
    file: UploadFile,
    thumbnail: Option<UploadFile>,
    format: SupportedImageFormat,
    description: Option<String>,
    focus: Option<(f64, f64)>,
) -> Result<roost_db::LocalMediaAttachment, MediaStoreError> {
    let media_id = Uuid::now_v7();
    let relative_path = relative_media_path(media_id, "", format.extension);
    let preview_path = relative_media_path(media_id, "small", "png");
    let full_path = media_path(state, &relative_path);
    let preview_full_path = media_path(state, &preview_path);
    let original_filename = file.name.clone();
    create_media_parent(&full_path).await?;
    create_media_parent(&preview_full_path).await?;

    let original_bytes = read_upload(file).await?;
    if original_bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(MediaStoreError::Validation("file is too large".to_owned()));
    }
    let thumbnail_bytes = read_optional_upload(thumbnail).await?;
    let processed =
        process_image(original_bytes.clone(), thumbnail_bytes, format.image_format).await?;

    tokio::fs::write(&full_path, &original_bytes).await?;
    tokio::fs::write(&preview_full_path, &processed.preview_bytes).await?;

    let media = roost_db::NewLocalMediaAttachment {
        account_id,
        content_type: format.content_type.to_owned(),
        original_filename,
        file_path: relative_path,
        preview_file_path: Some(preview_path),
        file_size: original_bytes.len() as i64,
        description,
        focus_x: focus.map(|focus| focus.0),
        focus_y: focus.map(|focus| focus.1),
        width: Some(processed.width),
        height: Some(processed.height),
        preview_width: Some(processed.preview_width),
        preview_height: Some(processed.preview_height),
        blurhash: Some(processed.blurhash),
    };
    roost_db::create_local_media_attachment(&state.db, media)
        .await
        .map_err(MediaStoreError::Roost)
}

/// Build replacement preview metadata from a custom thumbnail upload.
async fn replacement_preview(
    state: &AppState,
    media_id: Uuid,
    thumbnail: Option<UploadFile>,
) -> Result<Option<roost_db::LocalMediaPreviewUpdate>, MediaStoreError> {
    let Some(thumbnail) = thumbnail else {
        return Ok(None);
    };
    let format = validate_image_content_type(&thumbnail.content_type)
        .map_err(MediaStoreError::Validation)?;
    let preview_path = relative_media_path(media_id, "small", "png");
    let preview_full_path = media_path(state, &preview_path);
    create_media_parent(&preview_full_path).await?;
    let thumbnail_bytes = read_upload(thumbnail).await?;
    let processed = process_image(
        thumbnail_bytes.clone(),
        Some(thumbnail_bytes),
        format.image_format,
    )
    .await?;
    tokio::fs::write(&preview_full_path, &processed.preview_bytes).await?;

    Ok(Some(roost_db::LocalMediaPreviewUpdate {
        preview_file_path: preview_path,
        preview_width: processed.preview_width,
        preview_height: processed.preview_height,
        blurhash: processed.blurhash,
    }))
}

/// Read an optional multipart upload fully into memory for image processing.
async fn read_optional_upload(
    file: Option<UploadFile>,
) -> Result<Option<Vec<u8>>, MediaStoreError> {
    match file {
        Some(file) => Ok(Some(read_upload(file).await?)),
        None => Ok(None),
    }
}

/// Read a multipart upload fully into memory before blocking image processing.
async fn read_upload(file: UploadFile) -> Result<Vec<u8>, MediaStoreError> {
    let mut input = file.open().await?;
    let mut bytes = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut input, &mut bytes).await?;
    Ok(bytes)
}

/// Create the parent directory for a media file when it is nested by UUID shards.
async fn create_media_parent(path: &Path) -> Result<(), MediaStoreError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    Ok(())
}

/// Decode uploaded image bytes, generate a small PNG preview, and compute blurhash.
async fn process_image(
    original_bytes: Vec<u8>,
    thumbnail_bytes: Option<Vec<u8>>,
    original_format: ImageFormat,
) -> Result<ProcessedImage, MediaStoreError> {
    task::spawn_blocking(move || {
        let original = decode_image(&original_bytes, original_format)?;
        let (width, height) = original.dimensions();
        let preview_source = match thumbnail_bytes {
            Some(bytes) => decode_image_guessed(&bytes)?,
            None => original,
        };
        let preview = if preview_source.width() > PREVIEW_BOUNDING_BOX
            || preview_source.height() > PREVIEW_BOUNDING_BOX
        {
            preview_source.thumbnail(PREVIEW_BOUNDING_BOX, PREVIEW_BOUNDING_BOX)
        } else {
            preview_source
        };
        let (preview_width, preview_height) = preview.dimensions();
        let rgba = preview.to_rgba8();
        let blurhash = blurhash::encode(4, 3, preview_width, preview_height, rgba.as_raw())
            .map_err(|_| MediaStoreError::Validation("could not generate blurhash".to_owned()))?;
        let mut preview_bytes = Cursor::new(Vec::new());
        preview
            .write_to(&mut preview_bytes, ImageFormat::Png)
            .map_err(|_| MediaStoreError::Validation("could not generate thumbnail".to_owned()))?;

        Ok(ProcessedImage {
            width: width as i32,
            height: height as i32,
            preview_bytes: preview_bytes.into_inner(),
            preview_width: preview_width as i32,
            preview_height: preview_height as i32,
            blurhash,
        })
    })
    .await
    .map_err(|error| MediaStoreError::Roost(RoostError::InvalidInput(error.to_string())))?
}

/// Decode bytes using the MIME-derived image format accepted by upload validation.
fn decode_image(bytes: &[u8], format: ImageFormat) -> Result<image::DynamicImage, MediaStoreError> {
    image::load_from_memory_with_format(bytes, format)
        .map_err(|_| MediaStoreError::Validation("file is invalid".to_owned()))
}

/// Decode thumbnail bytes by sniffing their image format.
fn decode_image_guessed(bytes: &[u8]) -> Result<image::DynamicImage, MediaStoreError> {
    ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| MediaStoreError::Validation("file is invalid".to_owned()))?
        .decode()
        .map_err(|_| MediaStoreError::Validation("file is invalid".to_owned()))
}

/// Ensure media writes use the local filesystem backend implemented by this module.
fn ensure_local_storage(state: &AppState) -> Result<(), RoostError> {
    if state.config.object_storage_backend == "local" {
        Ok(())
    } else {
        Err(RoostError::Configuration(format!(
            "unsupported object storage backend: {}",
            state.config.object_storage_backend
        )))
    }
}

/// Validate a multipart upload content type against the advertised image formats.
fn validate_image_content_type(value: &str) -> Result<SupportedImageFormat, String> {
    SUPPORTED_IMAGE_FORMATS
        .iter()
        .copied()
        .find(|format| format.content_type == value)
        .ok_or_else(|| "file content type is invalid".to_owned())
}

/// Normalize optional alt text while enforcing Mastodon's media description limit.
fn normalize_description(value: Option<String>) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.chars().count() > DESCRIPTION_LIMIT {
        return Err("description is too long".to_owned());
    }
    let value = value.trim().to_owned();
    Ok((!value.is_empty()).then_some(value))
}

/// Parse Mastodon's comma-delimited focal point parameter.
fn parse_focus(value: Option<&str>) -> Result<Option<(f64, f64)>, String> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let Some((x, y)) = value.split_once(',') else {
        return Err("focus is invalid".to_owned());
    };
    let x = x
        .trim()
        .parse::<f64>()
        .map_err(|_| "focus is invalid".to_owned())?;
    let y = y
        .trim()
        .parse::<f64>()
        .map_err(|_| "focus is invalid".to_owned())?;
    if (-1.0..=1.0).contains(&x) && (-1.0..=1.0).contains(&y) {
        Ok(Some((x, y)))
    } else {
        Err("focus is invalid".to_owned())
    }
}

/// Build Mastodon-compatible structured metadata from stored local media metadata.
fn media_meta(media: &roost_db::LocalMediaAttachment) -> MediaMeta {
    MediaMeta {
        original: image_meta(media.width, media.height),
        small: image_meta(media.preview_width, media.preview_height),
        focus: match (media.focus_x, media.focus_y) {
            (Some(x), Some(y)) => Some(FocusMeta { x, y }),
            _ => None,
        },
    }
}

/// Build dimension metadata only when both dimensions are known.
fn image_meta(width: Option<i32>, height: Option<i32>) -> Option<ImageMeta> {
    let (Some(width), Some(height)) = (width, height) else {
        return None;
    };
    Some(ImageMeta {
        width,
        height,
        size: format!("{width}x{height}"),
        aspect: width as f64 / height as f64,
    })
}

/// Map upload MIME types to Mastodon media attachment type labels.
fn media_type(content_type: &str) -> &'static str {
    match content_type {
        "image/gif" => "gifv",
        value
            if SUPPORTED_IMAGE_FORMATS
                .iter()
                .any(|format| format.content_type == value) =>
        {
            "image"
        }
        _ => "unknown",
    }
}

/// Build a UUID-sharded path for original and preview media files.
fn relative_media_path(media_id: Uuid, variant: &str, extension: &str) -> String {
    let id = media_id.simple().to_string();
    let name = if variant.is_empty() {
        id.clone()
    } else {
        format!("{id}-{variant}")
    };
    format!("{}/{}/{}.{}", &id[0..2], &id[2..4], name, extension)
}

/// Infer a served media content type from the stored file extension.
fn content_type_from_path(path: &Path) -> &'static str {
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return "application/octet-stream";
    };
    SUPPORTED_IMAGE_FORMATS
        .iter()
        .find(|format| format.extension == extension)
        .map(|format| format.content_type)
        .unwrap_or("application/octet-stream")
}

/// Resolve a stored relative media path under the configured media root.
fn media_path(state: &AppState, relative_path: &str) -> PathBuf {
    Path::new(&state.config.media_root).join(relative_path)
}

/// Build the public URL for a stored media file.
fn media_url(state: &AppState, relative_path: &str) -> String {
    state
        .config
        .public_base_url
        .join(&format!("media_attachments/files/{relative_path}"))
        .map(|url| url.to_string())
        .unwrap_or_else(|_| {
            format!(
                "{}/media_attachments/files/{relative_path}",
                state.config.public_base_url
            )
        })
}

/// Reject absolute paths and parent traversal in public media file routes.
fn safe_relative_path(path: &str) -> Option<PathBuf> {
    let path = Path::new(path);
    if path.is_absolute() {
        return None;
    }
    let mut safe = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(segment) => safe.push(segment),
            _ => return None,
        }
    }
    Some(safe)
}

fn unprocessable(description: &str) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(ApiError::new(description)),
    )
        .into_response()
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ApiError::new("Record not found")),
    )
        .into_response()
}

fn server_error(error: RoostError) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiError::new(error.to_string())),
    )
        .into_response()
}

#[derive(Debug, thiserror::Error)]
enum MediaStoreError {
    #[error("{0}")]
    Validation(String),
    #[error(transparent)]
    Roost(#[from] RoostError),
}

impl From<std::io::Error> for MediaStoreError {
    fn from(error: std::io::Error) -> Self {
        Self::Roost(RoostError::from(error))
    }
}
