//! FEP-8b32 top-level `eddsa-jcs-2022` ActivityPub integrity proofs.

use std::{borrow::Cow, collections::HashSet, fmt};

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use multibase::Base;
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{Error as DeError, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub(super) const DATA_INTEGRITY_CONTEXT: &str = "https://w3id.org/security/data-integrity/v2";

/// The only proof representation accepted in this FEP phase.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) enum ProofType {
    DataIntegrityProof,
}

/// The canonical-JSON Ed25519 cryptosuite defined by W3C Data Integrity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) enum Cryptosuite {
    #[serde(rename = "eddsa-jcs-2022")]
    EddsaJcs2022,
}

/// The controller relationship authorized for ActivityPub object proofs.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum ProofPurpose {
    AssertionMethod,
}

/// A single top-level Data Integrity proof.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct IntegrityProof {
    #[serde(rename = "@context", skip_serializing_if = "Option::is_none")]
    context: Option<Value>,
    #[serde(rename = "type")]
    proof_type: ProofType,
    cryptosuite: Cryptosuite,
    created: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires: Option<String>,
    verification_method: String,
    proof_purpose: ProofPurpose,
    #[serde(skip_serializing_if = "Option::is_none")]
    proof_value: Option<String>,
}

/// Validated proof metadata needed to resolve its verification method.
#[derive(Debug)]
pub(super) struct PreparedProof {
    pub(super) verification_method: String,
    proof: IntegrityProof,
}

/// Stable verification outcomes used by bounded metrics and tracing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum VerificationOutcome {
    Unsupported,
    Invalid,
    Expired,
    Unresolved,
    ControllerMismatch,
}

impl VerificationOutcome {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::Invalid => "invalid",
            Self::Expired => "expired",
            Self::Unresolved => "unresolved",
            Self::ControllerMismatch => "controller_mismatch",
        }
    }
}

#[derive(Debug, Error)]
pub(super) enum IntegrityError {
    #[error("{0}")]
    Unsupported(Cow<'static, str>),
    #[error("{0}")]
    Invalid(Cow<'static, str>),
    #[error("integrity proof has expired")]
    Expired,
    #[error("integrity proof signature is invalid")]
    Cryptographic,
}

impl IntegrityError {
    pub(super) const fn outcome(&self) -> VerificationOutcome {
        match self {
            Self::Unsupported(_) => VerificationOutcome::Unsupported,
            Self::Expired => VerificationOutcome::Expired,
            Self::Invalid(_) | Self::Cryptographic => VerificationOutcome::Invalid,
        }
    }
}

/// Parse JSON while rejecting duplicate properties at every nesting level.
pub(super) fn parse_unique_json(bytes: &[u8]) -> Result<Value, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = UniqueValue::deserialize(&mut deserializer)?.0;
    deserializer.end()?;
    Ok(value)
}

struct UniqueValue(Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(UniqueVisitor)
    }
}

struct UniqueVisitor;

impl<'de> Visitor<'de> for UniqueVisitor {
    type Value = UniqueValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object properties")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Bool(value)))
    }
    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(UniqueValue(value.into()))
    }
    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(UniqueValue(value.into()))
    }
    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E> {
        Ok(UniqueValue(value.into()))
    }
    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: DeError,
    {
        self.visit_string(value.to_owned())
    }
    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::String(value)))
    }
    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Null))
    }
    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<UniqueValue>()? {
            values.push(value.0);
        }
        Ok(UniqueValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = HashSet::new();
        let mut values = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(A::Error::custom(format!("duplicate JSON property `{key}`")));
            }
            values.insert(key, object.next_value::<UniqueValue>()?.0);
        }
        Ok(UniqueValue(Value::Object(values)))
    }
}

/// Validate a single supported proof and return its key identifier.
pub(super) fn prepare(
    document: &Value,
    now: OffsetDateTime,
) -> Result<PreparedProof, IntegrityError> {
    let raw = document.get("proof").ok_or_else(|| {
        IntegrityError::Invalid("forwarded activity has no integrity proof".into())
    })?;
    if raw.is_array() {
        return Err(IntegrityError::Unsupported(
            "proof sets are unsupported".into(),
        ));
    }
    let proof: IntegrityProof = serde_json::from_value(raw.clone()).map_err(|_| {
        IntegrityError::Unsupported("unsupported or malformed integrity proof".into())
    })?;
    parse_date(&proof.created)?;
    if let Some(expires) = &proof.expires
        && parse_date(expires)? <= now
    {
        return Err(IntegrityError::Expired);
    }
    validate_context(document.get("@context"), proof.context.as_ref())?;
    if proof.proof_value.is_none() {
        return Err(IntegrityError::Invalid(
            "integrity proof has no proofValue".into(),
        ));
    }
    Ok(PreparedProof {
        verification_method: proof.verification_method.clone(),
        proof,
    })
}

/// Verify the prepared proof with an exact Ed25519 verification method.
pub(super) fn verify(
    document: &Value,
    prepared: &PreparedProof,
    public_key: &[u8],
) -> Result<(), IntegrityError> {
    let proof_value = prepared
        .proof
        .proof_value
        .as_deref()
        .ok_or_else(|| IntegrityError::Invalid("integrity proof has no proofValue".into()))?;
    let (base, signature_bytes) = multibase::decode(proof_value)
        .map_err(|_| IntegrityError::Invalid("proofValue is not Multibase".into()))?;
    if base != Base::Base58Btc || signature_bytes.len() != 64 {
        return Err(IntegrityError::Invalid(
            "proofValue is not a 64-byte base58-btc signature".into(),
        ));
    }
    let public_key: [u8; 32] = public_key
        .try_into()
        .map_err(|_| IntegrityError::Invalid("verification method is not an Ed25519 key".into()))?;
    let verifying_key = VerifyingKey::from_bytes(&public_key)
        .map_err(|_| IntegrityError::Invalid("verification method is not an Ed25519 key".into()))?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|_| IntegrityError::Invalid("proofValue is not an Ed25519 signature".into()))?;
    let hash_data = hash_data(document, &prepared.proof)?;
    verifying_key
        .verify_strict(&hash_data, &signature)
        .map_err(|_| IntegrityError::Cryptographic)
}

/// Add one stable top-level proof, replacing legacy authentication metadata.
pub(super) fn sign(
    document: &mut Value,
    verification_method: &str,
    created: OffsetDateTime,
    signing_key: &SigningKey,
) -> Result<(), IntegrityError> {
    {
        let object = document
            .as_object_mut()
            .ok_or_else(|| IntegrityError::Invalid("secured document is not an object".into()))?;
        object.remove("proof");
        object.remove("signature");
        add_context(object)?;
    }
    let mut proof = IntegrityProof {
        context: document.get("@context").cloned(),
        proof_type: ProofType::DataIntegrityProof,
        cryptosuite: Cryptosuite::EddsaJcs2022,
        created: created.format(&Rfc3339).map_err(|_| {
            IntegrityError::Invalid("proof creation date cannot be formatted".into())
        })?,
        expires: None,
        verification_method: verification_method.to_owned(),
        proof_purpose: ProofPurpose::AssertionMethod,
        proof_value: None,
    };
    let hash_data = hash_data(document, &proof)?;
    proof.proof_value = Some(multibase::encode(
        Base::Base58Btc,
        signing_key.sign(&hash_data).to_bytes(),
    ));
    document
        .as_object_mut()
        .ok_or_else(|| IntegrityError::Invalid("secured document is not an object".into()))?
        .insert(
            "proof".to_owned(),
            serde_json::to_value(proof)
                .map_err(|_| IntegrityError::Invalid("proof cannot be serialized".into()))?,
        );
    Ok(())
}

fn parse_date(value: &str) -> Result<OffsetDateTime, IntegrityError> {
    OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|_| IntegrityError::Invalid("integrity proof date is not RFC 3339".into()))
}

fn contexts(value: Option<&Value>) -> Result<Vec<&Value>, IntegrityError> {
    match value {
        Some(Value::Array(values)) => Ok(values.iter().collect()),
        Some(value @ (Value::String(_) | Value::Object(_))) => Ok(vec![value]),
        _ => Err(IntegrityError::Invalid(
            "integrity proof context is invalid".into(),
        )),
    }
}

fn validate_context(document: Option<&Value>, proof: Option<&Value>) -> Result<(), IntegrityError> {
    let Some(proof) = proof else {
        return Ok(());
    };
    let document = contexts(document)?;
    let proof = contexts(Some(proof))?;
    if proof.len() > document.len()
        || !proof
            .iter()
            .zip(document)
            .all(|(left, right)| left == &right)
    {
        return Err(IntegrityError::Invalid(
            "proof context is not a document context prefix".into(),
        ));
    }
    Ok(())
}

fn add_context(object: &mut Map<String, Value>) -> Result<(), IntegrityError> {
    match object.get_mut("@context") {
        Some(context @ Value::String(_)) if context.as_str() != Some(DATA_INTEGRITY_CONTEXT) => {
            let first = context.take();
            *context = Value::Array(vec![
                first,
                Value::String(DATA_INTEGRITY_CONTEXT.to_owned()),
            ]);
        }
        Some(Value::Array(contexts)) => {
            if !contexts
                .iter()
                .any(|value| value.as_str() == Some(DATA_INTEGRITY_CONTEXT))
            {
                contexts.push(Value::String(DATA_INTEGRITY_CONTEXT.to_owned()));
            }
        }
        Some(Value::String(_)) => {}
        None => {
            object.insert(
                "@context".to_owned(),
                Value::Array(vec![Value::String(DATA_INTEGRITY_CONTEXT.to_owned())]),
            );
        }
        _ => {
            return Err(IntegrityError::Invalid(
                "secured document context is invalid".into(),
            ));
        }
    }
    Ok(())
}

fn hash_data(document: &Value, proof: &IntegrityProof) -> Result<[u8; 64], IntegrityError> {
    let mut unsecured = document.clone();
    let object = unsecured
        .as_object_mut()
        .ok_or_else(|| IntegrityError::Invalid("secured document is not an object".into()))?;
    object.remove("proof");
    object.remove("signature");
    if let Some(context) = &proof.context {
        object.insert("@context".to_owned(), context.clone());
    }
    let mut proof_config = serde_json::to_value(proof)
        .map_err(|_| IntegrityError::Invalid("proof cannot be serialized".into()))?;
    proof_config
        .as_object_mut()
        .ok_or_else(|| IntegrityError::Invalid("proof is not an object".into()))?
        .remove("proofValue");
    let proof_bytes = serde_json_canonicalizer::to_vec(&proof_config).map_err(|_| {
        IntegrityError::Invalid("proof configuration cannot be canonicalized".into())
    })?;
    let document_bytes = serde_json_canonicalizer::to_vec(&unsecured)
        .map_err(|_| IntegrityError::Invalid("document cannot be canonicalized".into()))?;
    let proof_hash = Sha256::digest(proof_bytes);
    let document_hash = Sha256::digest(document_bytes);
    let mut result = [0_u8; 64];
    result[..32].copy_from_slice(&proof_hash);
    result[32..].copy_from_slice(&document_hash);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn generated_proof_round_trips_and_detects_changes() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let mut document = json!({"@context": "https://www.w3.org/ns/activitystreams", "id": "https://example.test/1", "type": "Create"});
        sign(
            &mut document,
            "https://example.test/users/alice#key",
            OffsetDateTime::UNIX_EPOCH,
            &key,
        )
        .unwrap();
        let prepared = prepare(&document, OffsetDateTime::UNIX_EPOCH).unwrap();
        verify(&document, &prepared, key.verifying_key().as_bytes()).unwrap();
        document["type"] = json!("Delete");
        assert!(matches!(
            verify(&document, &prepared, key.verifying_key().as_bytes()),
            Err(IntegrityError::Cryptographic)
        ));
    }

    /// The W3C `eddsa-jcs-2022` recommendation vector verifies byte-for-byte.
    #[test]
    fn verifies_w3c_eddsa_jcs_2022_vector() {
        let document = json!({
            "@context": [
                "https://www.w3.org/ns/credentials/v2",
                "https://www.w3.org/ns/credentials/examples/v2"
            ],
            "id": "urn:uuid:58172aac-d8ba-11ed-83dd-0b3aef56cc33",
            "type": ["VerifiableCredential", "AlumniCredential"],
            "name": "Alumni Credential",
            "description": "A minimum viable example of an Alumni Credential.",
            "issuer": "https://vc.example/issuers/5678",
            "validFrom": "2023-01-01T00:00:00Z",
            "credentialSubject": {
                "id": "did:example:abcdefgh",
                "alumniOf": "The School of Examples"
            },
            "proof": {
                "type": "DataIntegrityProof",
                "cryptosuite": "eddsa-jcs-2022",
                "created": "2023-02-24T23:36:38Z",
                "verificationMethod": "did:key:z6MkrJVnaZkeFzdQyMZu1cgjg7k1pZZ6pvBQ7XJPt4swbTQ2#z6MkrJVnaZkeFzdQyMZu1cgjg7k1pZZ6pvBQ7XJPt4swbTQ2",
                "proofPurpose": "assertionMethod",
                "@context": [
                    "https://www.w3.org/ns/credentials/v2",
                    "https://www.w3.org/ns/credentials/examples/v2"
                ],
                "proofValue": "z2HnFSSPPBzR36zdDgK8PbEHeXbR56YF24jwMpt3R1eHXQzJDMWS93FCzpvJpwTWd3GAVFuUfjoJdcnTMuVor51aX"
            }
        });
        let (_, multikey) =
            multibase::decode("z6MkrJVnaZkeFzdQyMZu1cgjg7k1pZZ6pvBQ7XJPt4swbTQ2").unwrap();
        assert_eq!(&multikey[..2], &[0xed, 0x01]);
        let prepared = prepare(&document, OffsetDateTime::UNIX_EPOCH).unwrap();
        verify(&document, &prepared, &multikey[2..]).unwrap();
    }

    #[test]
    fn duplicate_properties_are_rejected_recursively() {
        assert!(parse_unique_json(br#"{"object":{"id":1,"id":2}}"#).is_err());
    }

    #[test]
    fn jcs_handles_floating_point_and_utf16_property_order() {
        let value: Value =
            serde_json::from_str(r#"{"€":1,"\r":2,"😀":3,"a":333333333.33333329}"#).unwrap();
        assert_eq!(
            String::from_utf8(serde_json_canonicalizer::to_vec(&value).unwrap()).unwrap(),
            r#"{"\r":2,"a":333333333.3333333,"€":1,"😀":3}"#
        );
    }

    #[test]
    fn malformed_proof_shapes_contexts_dates_and_values_are_rejected() {
        let now = OffsetDateTime::parse("2026-08-24T12:00:00Z", &Rfc3339).unwrap();
        let base = json!({
            "@context": ["https://www.w3.org/ns/activitystreams", DATA_INTEGRITY_CONTEXT],
            "proof": {
                "@context": ["https://www.w3.org/ns/activitystreams", DATA_INTEGRITY_CONTEXT],
                "type": "DataIntegrityProof",
                "cryptosuite": "eddsa-jcs-2022",
                "created": "2026-08-24T10:00:00Z",
                "verificationMethod": "https://example.test/users/alice#key",
                "proofPurpose": "assertionMethod",
                "proofValue": "z111"
            }
        });
        assert!(prepare(&base, now).is_ok());

        let mut array = base.clone();
        array["proof"] = json!([array["proof"].clone()]);
        assert!(matches!(
            prepare(&array, now),
            Err(IntegrityError::Unsupported(_))
        ));

        let mut reordered = base.clone();
        reordered["proof"]["@context"] = json!([DATA_INTEGRITY_CONTEXT]);
        assert!(matches!(
            prepare(&reordered, now),
            Err(IntegrityError::Invalid(_))
        ));

        let mut expired = base.clone();
        expired["proof"]["expires"] = json!("2026-08-24T11:00:00Z");
        assert!(matches!(
            prepare(&expired, now),
            Err(IntegrityError::Expired)
        ));

        let prepared = prepare(&base, now).unwrap();
        assert!(matches!(
            verify(&base, &prepared, &[0; 32]),
            Err(IntegrityError::Invalid(_))
        ));
    }
}
