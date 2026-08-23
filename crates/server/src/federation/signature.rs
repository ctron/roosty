//! HTTP request signatures used by ActivityPub federation.

use std::{
    borrow::Cow,
    collections::{BTreeMap, HashSet},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, request::Parts};
use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature as Ed25519Signature, VerifyingKey as Ed25519VerifyingKey};
use roosty_db::ActorKeyAlgorithm;
use rsa::{
    RsaPrivateKey, RsaPublicKey,
    pkcs1::DecodeRsaPublicKey,
    pkcs1v15::{Signature as RsaSignature, SigningKey, VerifyingKey},
    pkcs8::{DecodePublicKey, EncodePublicKey, LineEnding},
    signature::{SignatureEncoding, Signer, Verifier},
};
use sfv::{BareItem, Dictionary, InnerList, ListEntry, ListSerializer, Parser, Version};
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

const SIGNATURE_LABEL: &str = "sig1";
const CLOCK_SKEW: Duration = Duration::from_secs(300);

/// HTTP signature wire format selected for a federation request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SignatureFormat {
    Legacy,
    Rfc9421,
}

impl SignatureFormat {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Rfc9421 => "rfc9421",
        }
    }
}

/// Signature algorithms accepted at the federation boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SignatureAlgorithm {
    RsaPkcs1Sha256,
    Ed25519,
}

impl SignatureAlgorithm {
    const fn as_str(self) -> &'static str {
        match self {
            Self::RsaPkcs1Sha256 => "rsa-v1_5-sha256",
            Self::Ed25519 => "ed25519",
        }
    }
}

/// Parsed identity needed to resolve the actor-owned verification key.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct SignatureIdentity {
    pub(crate) format: SignatureFormat,
    pub(crate) key_id: String,
}

/// Headers produced by signing one outbound request.
#[derive(Debug)]
pub(crate) struct SignedHeaders {
    pub(crate) format: SignatureFormat,
    pub(crate) headers: HeaderMap,
}

/// Typed signature parsing, canonicalization, digest, and cryptographic failures.
#[derive(Debug, Error)]
pub(crate) enum SignatureError {
    #[error("missing HTTP signature")]
    MissingSignature,
    #[error("malformed HTTP signature structured field")]
    MalformedStructuredField,
    #[error("HTTP signature must contain exactly one matching label")]
    InvalidLabels,
    #[error("HTTP signature parameter {0} is missing or invalid")]
    InvalidParameter(&'static str),
    #[error("unsupported HTTP signature algorithm")]
    UnsupportedAlgorithm,
    #[error("HTTP signature timestamp is outside the permitted window")]
    InvalidTimestamp,
    #[error("HTTP signature does not cover required components")]
    MissingRequiredComponent,
    #[error("unsupported HTTP signature component")]
    UnsupportedComponent,
    #[error("signed HTTP field is missing or invalid: {0}")]
    InvalidHeader(String),
    #[error("Content-Digest is missing or invalid")]
    InvalidContentDigest,
    #[error("HTTP signature key does not match the actor")]
    KeyMismatch,
    #[error("remote actor public key is invalid")]
    InvalidPublicKey,
    #[error("HTTP signature is invalid")]
    InvalidSignature,
    #[error("request target URI is invalid")]
    InvalidTargetUri,
}

/// Extract the signature format and key ID without trusting the requested algorithm.
pub(crate) fn identity(parts: &Parts) -> Result<Option<SignatureIdentity>, SignatureError> {
    if parts.headers.contains_key("signature-input") {
        let parsed = parse_rfc9421(parts)?;
        return Ok(Some(SignatureIdentity {
            format: SignatureFormat::Rfc9421,
            key_id: parsed.key_id,
        }));
    }
    let Some(value) = header_value(&parts.headers, "signature")? else {
        return Ok(None);
    };
    let key_id = legacy_attributes(value)
        .remove("keyId")
        .ok_or(SignatureError::InvalidParameter("keyid"))?;
    Ok(Some(SignatureIdentity {
        format: SignatureFormat::Legacy,
        key_id,
    }))
}

/// Verify a signed request against the actor-owned RSA key.
#[cfg(test)]
pub(crate) fn verify(
    parts: &Parts,
    body: Option<&[u8]>,
    public_base_url: &Url,
    expected_key_id: &str,
    public_key_pem: &str,
) -> Result<SignatureFormat, SignatureError> {
    if parts.headers.contains_key("signature-input") {
        verify_rfc9421(
            parts,
            body,
            public_base_url,
            expected_key_id,
            public_key_pem,
        )?;
        Ok(SignatureFormat::Rfc9421)
    } else {
        verify_legacy(parts, body, expected_key_id, public_key_pem)?;
        Ok(SignatureFormat::Legacy)
    }
}

/// Verify using the algorithm and canonical bytes selected from persisted actor-key metadata.
pub(crate) fn verify_actor_key(
    parts: &Parts,
    body: Option<&[u8]>,
    public_base_url: &Url,
    expected_key_id: &str,
    algorithm: ActorKeyAlgorithm,
    public_key: &[u8],
) -> Result<SignatureFormat, SignatureError> {
    if parts.headers.contains_key("signature-input") {
        verify_rfc9421_key(
            parts,
            body,
            public_base_url,
            expected_key_id,
            algorithm,
            public_key,
        )?;
        Ok(SignatureFormat::Rfc9421)
    } else {
        if algorithm != ActorKeyAlgorithm::RsaPkcs1Sha256 {
            return Err(SignatureError::UnsupportedAlgorithm);
        }
        verify_legacy_bytes(parts, body, expected_key_id, public_key)?;
        Ok(SignatureFormat::Legacy)
    }
}

/// Sign an outbound POST using either the legacy or RFC 9421 RSA profile.
pub(crate) fn sign_post(
    format: SignatureFormat,
    url: &Url,
    body: &[u8],
    private_key: &RsaPrivateKey,
    key_id: &str,
) -> Result<SignedHeaders, SignatureError> {
    match format {
        SignatureFormat::Legacy => sign_legacy_post(url, body, private_key, key_id),
        SignatureFormat::Rfc9421 => sign_rfc9421_post(url, body, private_key, key_id),
    }
}

fn verify_legacy(
    parts: &Parts,
    body: Option<&[u8]>,
    expected_key_id: &str,
    public_key_pem: &str,
) -> Result<(), SignatureError> {
    let date = header_value(&parts.headers, "date")?.ok_or(SignatureError::InvalidTimestamp)?;
    let date = httpdate::parse_http_date(date).map_err(|_| SignatureError::InvalidTimestamp)?;
    check_system_time(date)?;
    let signature =
        header_value(&parts.headers, "signature")?.ok_or(SignatureError::MissingSignature)?;
    let attributes = legacy_attributes(signature);
    if attributes.get("keyId").map(String::as_str) != Some(expected_key_id) {
        return Err(SignatureError::KeyMismatch);
    }
    let covered = attributes
        .get("headers")
        .map(String::as_str)
        .unwrap_or("(request-target)");
    let required = if body.is_some() {
        &["(request-target)", "host", "date", "digest"][..]
    } else {
        &["(request-target)", "host", "date"][..]
    };
    if required.iter().any(|required| {
        !covered
            .split_whitespace()
            .any(|name| name.eq_ignore_ascii_case(required))
    }) {
        return Err(SignatureError::MissingRequiredComponent);
    }
    let mut base = Vec::new();
    for name in covered.split_whitespace() {
        let value = if name.eq_ignore_ascii_case("(request-target)") {
            format!(
                "{} {}",
                parts.method.as_str().to_ascii_lowercase(),
                parts
                    .uri
                    .path_and_query()
                    .map(|value| value.as_str())
                    .unwrap_or("/")
            )
        } else {
            combined_header_value(&parts.headers, name)?
        };
        base.push(format!("{}: {value}", name.to_ascii_lowercase()));
    }
    if let Some(body) = body {
        let digest =
            header_value(&parts.headers, "digest")?.ok_or(SignatureError::InvalidContentDigest)?;
        let expected = format!("SHA-256={}", STANDARD.encode(Sha256::digest(body)));
        if digest != expected {
            return Err(SignatureError::InvalidContentDigest);
        }
    }
    let bytes = attributes
        .get("signature")
        .and_then(|value| STANDARD.decode(value).ok())
        .ok_or(SignatureError::InvalidSignature)?;
    verify_bytes(public_key_pem, base.join("\n").as_bytes(), &bytes)
}

struct ParsedRfc9421 {
    input: InnerList,
    components: Vec<String>,
    created: SystemTime,
    key_id: String,
    signature: Vec<u8>,
    algorithm: Option<SignatureAlgorithm>,
}

fn parse_rfc9421(parts: &Parts) -> Result<ParsedRfc9421, SignatureError> {
    let signature_input = combined_header_value(&parts.headers, "signature-input")?;
    let signature = combined_header_value(&parts.headers, "signature")?;
    let inputs: Dictionary = Parser::new(&signature_input)
        .with_version(Version::Rfc8941)
        .parse()
        .map_err(|_| SignatureError::MalformedStructuredField)?;
    let signatures: Dictionary = Parser::new(&signature)
        .with_version(Version::Rfc8941)
        .parse()
        .map_err(|_| SignatureError::MalformedStructuredField)?;
    if inputs.len() != 1 || signatures.len() != 1 {
        return Err(SignatureError::InvalidLabels);
    }
    let (label, entry) = inputs.first().ok_or(SignatureError::InvalidLabels)?;
    let (signature_label, signature_entry) =
        signatures.first().ok_or(SignatureError::InvalidLabels)?;
    if label != signature_label {
        return Err(SignatureError::InvalidLabels);
    }
    let ListEntry::InnerList(input) = entry else {
        return Err(SignatureError::MalformedStructuredField);
    };
    let ListEntry::Item(signature_item) = signature_entry else {
        return Err(SignatureError::MalformedStructuredField);
    };
    if !signature_item.params.is_empty() {
        return Err(SignatureError::MalformedStructuredField);
    }
    let signature = signature_item
        .bare_item
        .as_byte_sequence()
        .ok_or(SignatureError::MalformedStructuredField)?
        .to_vec();
    let created = input
        .params
        .get("created")
        .and_then(BareItem::as_integer)
        .ok_or(SignatureError::InvalidParameter("created"))?;
    let created: i64 = created.into();
    let created = u64::try_from(created)
        .ok()
        .and_then(|seconds| UNIX_EPOCH.checked_add(Duration::from_secs(seconds)))
        .ok_or(SignatureError::InvalidTimestamp)?;
    let key_id = input
        .params
        .get("keyid")
        .and_then(BareItem::as_string)
        .ok_or(SignatureError::InvalidParameter("keyid"))?
        .as_str()
        .to_owned();
    let algorithm = if let Some(algorithm) = input.params.get("alg") {
        let algorithm = algorithm
            .as_string()
            .ok_or(SignatureError::UnsupportedAlgorithm)?;
        Some(match algorithm.as_str() {
            "rsa-v1_5-sha256" => SignatureAlgorithm::RsaPkcs1Sha256,
            "ed25519" => SignatureAlgorithm::Ed25519,
            _ => return Err(SignatureError::UnsupportedAlgorithm),
        })
    } else {
        None
    };
    let mut seen = HashSet::new();
    let mut components = Vec::with_capacity(input.items.len());
    for item in &input.items {
        if !item.params.is_empty() {
            return Err(SignatureError::UnsupportedComponent);
        }
        let component = item
            .bare_item
            .as_string()
            .ok_or(SignatureError::UnsupportedComponent)?
            .as_str();
        if component != component.to_ascii_lowercase() || !seen.insert(component.to_owned()) {
            return Err(SignatureError::UnsupportedComponent);
        }
        if component.starts_with('@') && !matches!(component, "@method" | "@target-uri") {
            return Err(SignatureError::UnsupportedComponent);
        }
        if !component.starts_with('@') && HeaderName::from_bytes(component.as_bytes()).is_err() {
            return Err(SignatureError::UnsupportedComponent);
        }
        components.push(component.to_owned());
    }
    Ok(ParsedRfc9421 {
        input: input.clone(),
        components,
        created,
        key_id,
        signature,
        algorithm,
    })
}

fn verify_rfc9421_key(
    parts: &Parts,
    body: Option<&[u8]>,
    public_base_url: &Url,
    expected_key_id: &str,
    algorithm: ActorKeyAlgorithm,
    public_key: &[u8],
) -> Result<(), SignatureError> {
    let parsed = parse_rfc9421(parts)?;
    let expected = match algorithm {
        ActorKeyAlgorithm::RsaPkcs1Sha256 => SignatureAlgorithm::RsaPkcs1Sha256,
        ActorKeyAlgorithm::Ed25519 => SignatureAlgorithm::Ed25519,
    };
    if parsed.algorithm.is_some_and(|value| value != expected) {
        return Err(SignatureError::UnsupportedAlgorithm);
    }
    verify_rfc9421_parsed(
        parts,
        body,
        public_base_url,
        expected_key_id,
        &parsed,
        |base, signature| verify_key_bytes(algorithm, public_key, base, signature),
    )
}

#[cfg(test)]
fn verify_rfc9421(
    parts: &Parts,
    body: Option<&[u8]>,
    public_base_url: &Url,
    expected_key_id: &str,
    public_key_pem: &str,
) -> Result<(), SignatureError> {
    let parsed = parse_rfc9421(parts)?;
    if parsed
        .algorithm
        .is_some_and(|algorithm| algorithm != SignatureAlgorithm::RsaPkcs1Sha256)
    {
        return Err(SignatureError::UnsupportedAlgorithm);
    }
    verify_rfc9421_parsed(
        parts,
        body,
        public_base_url,
        expected_key_id,
        &parsed,
        |base, signature| verify_bytes(public_key_pem, base, signature),
    )
}

fn verify_rfc9421_parsed(
    parts: &Parts,
    body: Option<&[u8]>,
    public_base_url: &Url,
    expected_key_id: &str,
    parsed: &ParsedRfc9421,
    verify_signature: impl FnOnce(&[u8], &[u8]) -> Result<(), SignatureError>,
) -> Result<(), SignatureError> {
    check_system_time(parsed.created)?;
    if parsed.key_id != expected_key_id {
        return Err(SignatureError::KeyMismatch);
    }
    for required in ["@method", "@target-uri"] {
        if !parsed
            .components
            .iter()
            .any(|component| component == required)
        {
            return Err(SignatureError::MissingRequiredComponent);
        }
    }
    if let Some(body) = body {
        if !parsed
            .components
            .iter()
            .any(|component| component == "content-digest")
        {
            return Err(SignatureError::MissingRequiredComponent);
        }
        validate_content_digest(parts, body)?;
    }
    let target_uri = inbound_target_uri(public_base_url, parts)?;
    let signature_params = serialize_inner_list(&parsed.input);
    let base = signature_base(
        &parts.method,
        &target_uri,
        &parts.headers,
        &parsed.components,
        &signature_params,
    )?;
    verify_signature(base.as_bytes(), &parsed.signature)
}

fn sign_legacy_post(
    url: &Url,
    body: &[u8],
    private_key: &RsaPrivateKey,
    key_id: &str,
) -> Result<SignedHeaders, SignatureError> {
    let host = authority(url)?;
    let digest = format!("SHA-256={}", STANDARD.encode(Sha256::digest(body)));
    let date = httpdate::fmt_http_date(SystemTime::now());
    let path = url.path_and_query();
    let base =
        format!("(request-target): post {path}\nhost: {host}\ndate: {date}\ndigest: {digest}");
    let signature = SigningKey::<Sha256>::new(private_key.clone()).sign(base.as_bytes());
    let signature = format!(
        "keyId=\"{key_id}\",algorithm=\"rsa-sha256\",headers=\"(request-target) host date digest\",signature=\"{}\"",
        STANDARD.encode(signature.to_vec())
    );
    let mut headers = HeaderMap::new();
    insert_header(&mut headers, "date", &date)?;
    insert_header(&mut headers, "digest", &digest)?;
    insert_header(&mut headers, "signature", &signature)?;
    Ok(SignedHeaders {
        format: SignatureFormat::Legacy,
        headers,
    })
}

fn sign_rfc9421_post(
    url: &Url,
    body: &[u8],
    private_key: &RsaPrivateKey,
    key_id: &str,
) -> Result<SignedHeaders, SignatureError> {
    let content_digest = format!("sha-256=:{}:", STANDARD.encode(Sha256::digest(body)));
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| SignatureError::InvalidTimestamp)?
        .as_secs();
    let signature_params = format!(
        "(\"@method\" \"@target-uri\" \"content-digest\");created={created};keyid=\"{}\";alg=\"{}\"",
        escape_string(key_id),
        SignatureAlgorithm::RsaPkcs1Sha256.as_str(),
    );
    let mut base_headers = HeaderMap::new();
    insert_header(&mut base_headers, "content-digest", &content_digest)?;
    let components = [
        "@method".to_owned(),
        "@target-uri".to_owned(),
        "content-digest".to_owned(),
    ];
    let base = signature_base(
        &Method::POST,
        url.as_str(),
        &base_headers,
        &components,
        &signature_params,
    )?;
    let signature = SigningKey::<Sha256>::new(private_key.clone()).sign(base.as_bytes());
    let signature = format!(
        "{SIGNATURE_LABEL}=:{}:",
        STANDARD.encode(signature.to_vec())
    );
    let signature_input = format!("{SIGNATURE_LABEL}={signature_params}");
    insert_header(&mut base_headers, "signature-input", &signature_input)?;
    insert_header(&mut base_headers, "signature", &signature)?;
    Ok(SignedHeaders {
        format: SignatureFormat::Rfc9421,
        headers: base_headers,
    })
}

fn signature_base(
    method: &Method,
    target_uri: &str,
    headers: &HeaderMap,
    components: &[String],
    signature_params: &str,
) -> Result<String, SignatureError> {
    let mut lines = Vec::with_capacity(components.len() + 1);
    for component in components {
        let value = match component.as_str() {
            "@method" => method.as_str().to_ascii_uppercase(),
            "@target-uri" => target_uri.to_owned(),
            ordinary if !ordinary.starts_with('@') => combined_header_value(headers, ordinary)?,
            _ => return Err(SignatureError::UnsupportedComponent),
        };
        lines.push(format!("\"{component}\": {value}"));
    }
    lines.push(format!("\"@signature-params\": {signature_params}"));
    Ok(lines.join("\n"))
}

fn validate_content_digest(parts: &Parts, body: &[u8]) -> Result<(), SignatureError> {
    let value = combined_header_value(&parts.headers, "content-digest")
        .map_err(|_| SignatureError::InvalidContentDigest)?;
    let digest: Dictionary = Parser::new(&value)
        .with_version(Version::Rfc8941)
        .parse()
        .map_err(|_| SignatureError::InvalidContentDigest)?;
    let ListEntry::Item(item) = digest
        .get("sha-256")
        .ok_or(SignatureError::InvalidContentDigest)?
    else {
        return Err(SignatureError::InvalidContentDigest);
    };
    if !item.params.is_empty()
        || item.bare_item.as_byte_sequence() != Some(Sha256::digest(body).as_slice())
    {
        return Err(SignatureError::InvalidContentDigest);
    }
    Ok(())
}

fn inbound_target_uri(base: &Url, parts: &Parts) -> Result<String, SignatureError> {
    let mut target = base.clone();
    target.set_path(parts.uri.path());
    target.set_query(parts.uri.query());
    target.set_fragment(None);
    Ok(target.to_string())
}

fn verify_bytes(public_key_pem: &str, base: &[u8], signature: &[u8]) -> Result<(), SignatureError> {
    let public_key = RsaPublicKey::from_public_key_pem(public_key_pem)
        .map_err(|_| SignatureError::InvalidPublicKey)?;
    let signature =
        RsaSignature::try_from(signature).map_err(|_| SignatureError::InvalidSignature)?;
    VerifyingKey::<Sha256>::new(public_key)
        .verify(base, &signature)
        .map_err(|_| SignatureError::InvalidSignature)
}

fn verify_legacy_bytes(
    parts: &Parts,
    body: Option<&[u8]>,
    expected_key_id: &str,
    public_key: &[u8],
) -> Result<(), SignatureError> {
    let key =
        RsaPublicKey::from_pkcs1_der(public_key).map_err(|_| SignatureError::InvalidPublicKey)?;
    let pem = key
        .to_public_key_pem(LineEnding::LF)
        .map_err(|_| SignatureError::InvalidPublicKey)?;
    verify_legacy(parts, body, expected_key_id, &pem)
}

fn verify_key_bytes(
    algorithm: ActorKeyAlgorithm,
    public_key: &[u8],
    base: &[u8],
    signature: &[u8],
) -> Result<(), SignatureError> {
    match algorithm {
        ActorKeyAlgorithm::RsaPkcs1Sha256 => {
            let public_key = RsaPublicKey::from_pkcs1_der(public_key)
                .map_err(|_| SignatureError::InvalidPublicKey)?;
            let signature =
                RsaSignature::try_from(signature).map_err(|_| SignatureError::InvalidSignature)?;
            VerifyingKey::<Sha256>::new(public_key)
                .verify(base, &signature)
                .map_err(|_| SignatureError::InvalidSignature)
        }
        ActorKeyAlgorithm::Ed25519 => {
            let bytes: &[u8; 32] = public_key
                .try_into()
                .map_err(|_| SignatureError::InvalidPublicKey)?;
            let public_key = Ed25519VerifyingKey::from_bytes(bytes)
                .map_err(|_| SignatureError::InvalidPublicKey)?;
            let signature = Ed25519Signature::try_from(signature)
                .map_err(|_| SignatureError::InvalidSignature)?;
            public_key
                .verify_strict(base, &signature)
                .map_err(|_| SignatureError::InvalidSignature)
        }
    }
}

fn check_system_time(timestamp: SystemTime) -> Result<(), SignatureError> {
    let now = SystemTime::now();
    let skew = now
        .duration_since(timestamp)
        .or_else(|_| timestamp.duration_since(now))
        .map_err(|_| SignatureError::InvalidTimestamp)?;
    if skew > CLOCK_SKEW {
        return Err(SignatureError::InvalidTimestamp);
    }
    Ok(())
}

fn serialize_inner_list(inner: &InnerList) -> String {
    let mut serializer = ListSerializer::new();
    let mut list = serializer.inner_list();
    list.items(&inner.items);
    let _ = list.finish().parameters(&inner.params).finish();
    serializer.finish().unwrap_or_default()
}

fn combined_header_value(headers: &HeaderMap, name: &str) -> Result<String, SignatureError> {
    let values = headers
        .get_all(name)
        .iter()
        .map(|value| {
            value
                .to_str()
                .map(|value| value.trim_matches([' ', '\t']))
                .map(str::to_owned)
                .map_err(|_| SignatureError::InvalidHeader(name.to_owned()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if values.is_empty() {
        return Err(SignatureError::InvalidHeader(name.to_owned()));
    }
    Ok(values.join(", "))
}

fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, SignatureError> {
    headers
        .get(name)
        .map(|value| {
            value
                .to_str()
                .map_err(|_| SignatureError::InvalidHeader(name.to_owned()))
        })
        .transpose()
}

fn legacy_attributes(value: &str) -> BTreeMap<String, String> {
    value
        .split(',')
        .filter_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            Some((key.to_owned(), value.trim_matches('"').to_owned()))
        })
        .collect()
}

fn authority(url: &Url) -> Result<String, SignatureError> {
    url.host_str().ok_or(SignatureError::InvalidTargetUri)?;
    Ok(url[url::Position::BeforeHost..url::Position::AfterPort].to_owned())
}

trait UrlRequestTarget {
    fn path_and_query(&self) -> Cow<'_, str>;
}

impl UrlRequestTarget for Url {
    fn path_and_query(&self) -> Cow<'_, str> {
        match self.query() {
            Some(query) => Cow::Owned(format!("{}?{query}", self.path())),
            None => Cow::Borrowed(self.path()),
        }
    }
}

fn insert_header(
    headers: &mut HeaderMap,
    name: &'static str,
    value: &str,
) -> Result<(), SignatureError> {
    let value =
        HeaderValue::from_str(value).map_err(|_| SignatureError::InvalidHeader(name.to_owned()))?;
    headers.insert(HeaderName::from_static(name), value);
    Ok(())
}

fn escape_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{HeaderMap, Method, Request},
    };
    use ed25519_dalek::{Signer as _, SigningKey as Ed25519SigningKey};
    use rand_core::OsRng;
    use rsa::{
        RsaPrivateKey,
        pkcs8::{EncodePublicKey, LineEnding},
        signature::{SignatureEncoding, Signer},
    };

    use super::*;

    fn request_parts(method: Method, uri: &str, headers: HeaderMap) -> Parts {
        let (mut parts, _) = Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .unwrap()
            .into_parts();
        parts.headers = headers;
        parts
    }

    fn custom_rfc_headers(
        method: &Method,
        url: &Url,
        body: Option<&[u8]>,
        private_key: &RsaPrivateKey,
        key_id: &str,
        components: &[String],
        parameters: &str,
    ) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            "content-type",
            HeaderValue::from_static("application/activity+json"),
        );
        if let Some(body) = body {
            insert_header(
                &mut headers,
                "content-digest",
                &format!("sha-256=:{}:", STANDARD.encode(Sha256::digest(body))),
            )
            .unwrap();
        }
        let component_list = components
            .iter()
            .map(|component| format!("\"{component}\""))
            .collect::<Vec<_>>()
            .join(" ");
        let signature_params = format!("({component_list}){parameters}");
        let base = signature_base(
            method,
            url.as_str(),
            &headers,
            components,
            &signature_params,
        )
        .unwrap();
        let signed = SigningKey::<Sha256>::new(private_key.clone()).sign(base.as_bytes());
        insert_header(
            &mut headers,
            "signature-input",
            &format!("sig1={signature_params}"),
        )
        .unwrap();
        insert_header(
            &mut headers,
            "signature",
            &format!("sig1=:{}:", STANDARD.encode(signed.to_vec())),
        )
        .unwrap();
        let _ = key_id;
        headers
    }

    fn fixture() -> (RsaPrivateKey, String, Url, String) {
        let private_key = RsaPrivateKey::new(&mut OsRng, 2048).unwrap();
        let public_key = private_key
            .to_public_key()
            .to_public_key_pem(LineEnding::LF)
            .unwrap();
        (
            private_key,
            public_key,
            "https://local.test/users/alice/inbox?shared=true"
                .parse()
                .unwrap(),
            "https://remote.test/users/bob#main-key".to_owned(),
        )
    }

    fn current_parameters(key_id: &str) -> String {
        let created = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        format!(";created={created};keyid=\"{key_id}\"")
    }

    /// Given a persisted Ed25519 method, RFC 9421 accepts matching or omitted `alg` and rejects RSA.
    #[test]
    fn verifies_ed25519_from_persisted_algorithm() {
        let signing_key = Ed25519SigningKey::from_bytes(&[7_u8; 32]);
        let key_id = "https://remote.test/users/bob#ed25519-1";
        let url: Url = "https://local.test/users/alice/outbox?page=true"
            .parse()
            .unwrap();
        for alg in [Some("ed25519"), None] {
            let created = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let suffix = alg.map_or_else(String::new, |alg| format!(";alg=\"{alg}\""));
            let params = format!(
                "(\"@method\" \"@target-uri\");created={created};keyid=\"{key_id}\"{suffix}"
            );
            let base = signature_base(
                &Method::GET,
                url.as_str(),
                &HeaderMap::new(),
                &["@method".to_owned(), "@target-uri".to_owned()],
                &params,
            )
            .unwrap();
            let signature = signing_key.sign(base.as_bytes());
            let mut headers = HeaderMap::new();
            insert_header(&mut headers, "signature-input", &format!("sig1={params}")).unwrap();
            insert_header(
                &mut headers,
                "signature",
                &format!("sig1=:{}:", STANDARD.encode(signature.to_bytes())),
            )
            .unwrap();
            let parts = request_parts(Method::GET, "/users/alice/outbox?page=true", headers);
            assert_eq!(
                verify_actor_key(
                    &parts,
                    None,
                    &"https://local.test".parse().unwrap(),
                    key_id,
                    ActorKeyAlgorithm::Ed25519,
                    &signing_key.verifying_key().to_bytes()
                )
                .unwrap(),
                SignatureFormat::Rfc9421
            );
        }
    }

    /// Given valid RSA requests, both RFC POST and digest-free RFC GET profiles verify.
    #[test]
    fn verifies_rfc9421_post_and_get() {
        let (private_key, public_key, post_url, key_id) = fixture();
        let body = br#"{"type":"Create"}"#;
        let signed = sign_post(
            SignatureFormat::Rfc9421,
            &post_url,
            body,
            &private_key,
            &key_id,
        )
        .unwrap();
        let post = request_parts(
            Method::POST,
            "/users/alice/inbox?shared=true",
            signed.headers,
        );
        assert_eq!(
            verify(
                &post,
                Some(body),
                &"https://local.test".parse().unwrap(),
                &key_id,
                &public_key,
            )
            .unwrap(),
            SignatureFormat::Rfc9421
        );

        let get_url: Url = "https://local.test/users/alice/outbox?page=true"
            .parse()
            .unwrap();
        let components = vec!["@method".to_owned(), "@target-uri".to_owned()];
        let headers = custom_rfc_headers(
            &Method::GET,
            &get_url,
            None,
            &private_key,
            &key_id,
            &components,
            &current_parameters(&key_id),
        );
        let get = request_parts(Method::GET, "/users/alice/outbox?page=true", headers);
        assert!(
            verify(
                &get,
                None,
                &"https://local.test".parse().unwrap(),
                &key_id,
                &public_key,
            )
            .is_ok()
        );
    }

    /// Given a valid RFC request, changes to its method, target, body, digest, signature, or key fail.
    #[test]
    fn rejects_tampered_rfc9421_requests() {
        let (private_key, public_key, url, key_id) = fixture();
        let body = br#"{"type":"Create"}"#;
        let make_parts = || {
            let signed =
                sign_post(SignatureFormat::Rfc9421, &url, body, &private_key, &key_id).unwrap();
            request_parts(
                Method::POST,
                "/users/alice/inbox?shared=true",
                signed.headers,
            )
        };
        let base: Url = "https://local.test".parse().unwrap();

        let mut method = make_parts();
        method.method = Method::PUT;
        assert!(verify(&method, Some(body), &base, &key_id, &public_key).is_err());

        let mut target = make_parts();
        target.uri = "/users/alice/inbox?shared=false".parse().unwrap();
        assert!(verify(&target, Some(body), &base, &key_id, &public_key).is_err());

        assert!(verify(&make_parts(), Some(b"altered"), &base, &key_id, &public_key).is_err());

        let mut digest = make_parts();
        digest.headers.insert(
            "content-digest",
            HeaderValue::from_static("sha-256=:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=:"),
        );
        assert!(verify(&digest, Some(body), &base, &key_id, &public_key).is_err());

        let mut signature = make_parts();
        signature
            .headers
            .insert("signature", HeaderValue::from_static("sig1=:AA==:"));
        assert!(verify(&signature, Some(body), &base, &key_id, &public_key).is_err());
        assert!(
            verify(
                &make_parts(),
                Some(body),
                &base,
                "different-key",
                &public_key
            )
            .is_err()
        );
    }

    /// Given malformed or out-of-profile structured fields, parsing fails without legacy fallback.
    #[test]
    fn rejects_invalid_rfc9421_profile() {
        let (private_key, public_key, url, key_id) = fixture();
        let body = b"{}";
        let required = vec![
            "@method".to_owned(),
            "@target-uri".to_owned(),
            "content-digest".to_owned(),
            "content-type".to_owned(),
        ];
        let base: Url = "https://local.test".parse().unwrap();
        let verify_headers = |headers| {
            let parts = request_parts(Method::POST, "/users/alice/inbox?shared=true", headers);
            verify(&parts, Some(body), &base, &key_id, &public_key)
        };

        let stale = UNIX_EPOCH + Duration::from_secs(1);
        let stale_seconds = stale.duration_since(UNIX_EPOCH).unwrap().as_secs();
        let headers = custom_rfc_headers(
            &Method::POST,
            &url,
            Some(body),
            &private_key,
            &key_id,
            &required,
            &format!(";created={stale_seconds};keyid=\"{key_id}\""),
        );
        assert!(matches!(
            verify_headers(headers),
            Err(SignatureError::InvalidTimestamp)
        ));

        let future = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 600;
        let headers = custom_rfc_headers(
            &Method::POST,
            &url,
            Some(body),
            &private_key,
            &key_id,
            &required,
            &format!(";created={future};keyid=\"{key_id}\""),
        );
        assert!(matches!(
            verify_headers(headers),
            Err(SignatureError::InvalidTimestamp)
        ));

        let missing = vec!["@method".to_owned(), "content-digest".to_owned()];
        let headers = custom_rfc_headers(
            &Method::POST,
            &url,
            Some(body),
            &private_key,
            &key_id,
            &missing,
            &current_parameters(&key_id),
        );
        assert!(matches!(
            verify_headers(headers),
            Err(SignatureError::MissingRequiredComponent)
        ));

        let headers = custom_rfc_headers(
            &Method::POST,
            &url,
            Some(body),
            &private_key,
            &key_id,
            &required,
            &format!(";keyid=\"{key_id}\""),
        );
        assert!(matches!(
            verify_headers(headers),
            Err(SignatureError::InvalidParameter("created"))
        ));

        let created = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let headers = custom_rfc_headers(
            &Method::POST,
            &url,
            Some(body),
            &private_key,
            &key_id,
            &required,
            &format!(";created={created}"),
        );
        assert!(matches!(
            verify_headers(headers),
            Err(SignatureError::InvalidParameter("keyid"))
        ));

        let mut unsupported =
            sign_post(SignatureFormat::Rfc9421, &url, body, &private_key, &key_id)
                .unwrap()
                .headers;
        let input = unsupported["signature-input"]
            .to_str()
            .unwrap()
            .replace("rsa-v1_5-sha256", "ed25519");
        unsupported.insert("signature-input", HeaderValue::from_str(&input).unwrap());
        assert!(matches!(
            verify_headers(unsupported),
            Err(SignatureError::UnsupportedAlgorithm)
        ));

        let mut component_parameters =
            sign_post(SignatureFormat::Rfc9421, &url, body, &private_key, &key_id)
                .unwrap()
                .headers;
        let input = component_parameters["signature-input"]
            .to_str()
            .unwrap()
            .replacen("\"@method\"", "\"@method\";foo", 1);
        component_parameters.insert("signature-input", HeaderValue::from_str(&input).unwrap());
        assert!(matches!(
            verify_headers(component_parameters),
            Err(SignatureError::UnsupportedComponent)
        ));

        let mut altered_parameters =
            sign_post(SignatureFormat::Rfc9421, &url, body, &private_key, &key_id)
                .unwrap()
                .headers;
        let input = altered_parameters["signature-input"]
            .to_str()
            .unwrap()
            .replace(";alg=", ";nonce=\"changed\";alg=");
        altered_parameters.insert("signature-input", HeaderValue::from_str(&input).unwrap());
        assert!(matches!(
            verify_headers(altered_parameters),
            Err(SignatureError::InvalidSignature)
        ));

        let mut multiple = sign_post(SignatureFormat::Rfc9421, &url, body, &private_key, &key_id)
            .unwrap()
            .headers;
        let input = multiple["signature-input"].to_str().unwrap().to_owned();
        multiple.insert(
            "signature-input",
            HeaderValue::from_str(&format!("{input}, sig2=(\"@method\")")).unwrap(),
        );
        assert!(matches!(
            verify_headers(multiple),
            Err(SignatureError::InvalidLabels)
        ));

        let mut malformed = HeaderMap::new();
        malformed.insert(
            "signature-input",
            HeaderValue::from_static("not structured"),
        );
        malformed.insert("signature", HeaderValue::from_static("legacy-looking"));
        assert!(identity(&request_parts(Method::POST, "/", malformed)).is_err());
    }
}
