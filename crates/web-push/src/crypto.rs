use aes_gcm::{
    Aes128Gcm, KeyInit,
    aead::{Aead, Generate},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hkdf::Hkdf;
use p256::{PublicKey, ecdh::EphemeralSecret, elliptic_curve::sec1::ToSec1Point};
use sha2_v11::Sha256;

use crate::{Encoding, Subscription, WebPushError};

const RECORD_SIZE: u32 = 4096;

pub(crate) struct EncryptedPayload {
    pub content_encoding: &'static str,
    pub body: Vec<u8>,
    pub extra_headers: Vec<(&'static str, String)>,
}

pub(crate) fn encrypt(
    subscription: &Subscription,
    plaintext: &[u8],
) -> Result<EncryptedPayload, WebPushError> {
    let receiver = PublicKey::from_sec1_bytes(&subscription.p256dh)
        .map_err(|_| WebPushError::InvalidSubscriberKey)?;
    let sender_secret = EphemeralSecret::try_generate().map_err(|_| WebPushError::Encryption)?;
    let sender = sender_secret.public_key();
    let shared = sender_secret.diffie_hellman(&receiver);
    let sender_bytes = sender.to_sec1_point(false);
    let salt = <[u8; 16]>::try_generate().map_err(|_| WebPushError::Encryption)?;

    match subscription.encoding {
        Encoding::Aes128Gcm => encrypt_standard(
            &subscription.p256dh,
            sender_bytes.as_bytes(),
            shared.raw_secret_bytes().as_slice(),
            &subscription.auth,
            salt,
            plaintext,
        ),
        Encoding::AesGcm => encrypt_legacy(
            &subscription.p256dh,
            sender_bytes.as_bytes(),
            shared.raw_secret_bytes().as_slice(),
            &subscription.auth,
            salt,
            plaintext,
        ),
    }
}

fn encrypt_standard(
    receiver: &[u8],
    sender: &[u8],
    shared: &[u8],
    auth: &[u8],
    salt: [u8; 16],
    plaintext: &[u8],
) -> Result<EncryptedPayload, WebPushError> {
    if plaintext.len() > 3993 {
        return Err(WebPushError::PayloadTooLarge);
    }
    let mut info = b"WebPush: info\0".to_vec();
    info.extend_from_slice(receiver);
    info.extend_from_slice(sender);
    let auth_hkdf = Hkdf::<Sha256>::new(Some(auth), shared);
    let mut ikm = [0_u8; 32];
    auth_hkdf
        .expand(&info, &mut ikm)
        .map_err(|_| WebPushError::Encryption)?;
    let (key, nonce) = content_key_nonce(
        &salt,
        &ikm,
        b"Content-Encoding: aes128gcm\0",
        b"Content-Encoding: nonce\0",
    )?;
    let mut padded = plaintext.to_vec();
    padded.push(2);
    let body_ciphertext = Aes128Gcm::new_from_slice(&key)
        .map_err(|_| WebPushError::Encryption)?
        .encrypt((&nonce).into(), padded.as_slice())
        .map_err(|_| WebPushError::Encryption)?;
    let mut body = Vec::with_capacity(21 + sender.len() + body_ciphertext.len());
    body.extend_from_slice(&salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(sender.len() as u8);
    body.extend_from_slice(sender);
    body.extend_from_slice(&body_ciphertext);
    Ok(EncryptedPayload {
        content_encoding: "aes128gcm",
        body,
        extra_headers: Vec::new(),
    })
}

fn encrypt_legacy(
    receiver: &[u8],
    sender: &[u8],
    shared: &[u8],
    auth: &[u8],
    salt: [u8; 16],
    plaintext: &[u8],
) -> Result<EncryptedPayload, WebPushError> {
    if plaintext.len() > 4078 {
        return Err(WebPushError::PayloadTooLarge);
    }
    let auth_hkdf = Hkdf::<Sha256>::new(Some(auth), shared);
    let mut ikm = [0_u8; 32];
    auth_hkdf
        .expand(b"Content-Encoding: auth\0", &mut ikm)
        .map_err(|_| WebPushError::Encryption)?;

    let mut context = b"P-256\0".to_vec();
    context.extend_from_slice(&(receiver.len() as u16).to_be_bytes());
    context.extend_from_slice(receiver);
    context.extend_from_slice(&(sender.len() as u16).to_be_bytes());
    context.extend_from_slice(sender);
    let mut key_label = b"Content-Encoding: aesgcm\0".to_vec();
    key_label.extend_from_slice(&context);
    let mut nonce_label = b"Content-Encoding: nonce\0".to_vec();
    nonce_label.extend_from_slice(&context);
    let (key, nonce) = content_key_nonce(&salt, &ikm, &key_label, &nonce_label)?;
    let mut padded = Vec::with_capacity(plaintext.len() + 2);
    padded.extend_from_slice(&0_u16.to_be_bytes());
    padded.extend_from_slice(plaintext);
    let body = Aes128Gcm::new_from_slice(&key)
        .map_err(|_| WebPushError::Encryption)?
        .encrypt((&nonce).into(), padded.as_slice())
        .map_err(|_| WebPushError::Encryption)?;
    Ok(EncryptedPayload {
        content_encoding: "aesgcm",
        body,
        extra_headers: vec![
            (
                "Encryption",
                format!("salt={};rs={RECORD_SIZE}", URL_SAFE_NO_PAD.encode(salt)),
            ),
            (
                "Crypto-Key",
                format!("dh={}", URL_SAFE_NO_PAD.encode(sender)),
            ),
        ],
    })
}

fn content_key_nonce(
    salt: &[u8],
    ikm: &[u8],
    key_info: &[u8],
    nonce_info: &[u8],
) -> Result<([u8; 16], [u8; 12]), WebPushError> {
    let hkdf = Hkdf::<Sha256>::new(Some(salt), ikm);
    let mut key = [0_u8; 16];
    let mut nonce = [0_u8; 12];
    hkdf.expand(key_info, &mut key)
        .map_err(|_| WebPushError::Encryption)?;
    hkdf.expand(nonce_info, &mut nonce)
        .map_err(|_| WebPushError::Encryption)?;
    Ok((key, nonce))
}

#[cfg(test)]
mod tests {
    use super::{encrypt_legacy, encrypt_standard};
    use crate::WebPushError;
    use p256::{PublicKey, SecretKey, ecdh::diffie_hellman};

    fn bytes(value: &str) -> Vec<u8> {
        hex::decode(value)
            .unwrap_or_else(|error| unreachable!("fixed test vector is valid: {error}"))
    }

    fn shared_secret(private_key: &str, remote_public_key: &[u8]) -> Vec<u8> {
        let secret = SecretKey::from_slice(&bytes(private_key))
            .unwrap_or_else(|error| unreachable!("fixed private key is valid: {error}"));
        let remote = PublicKey::from_sec1_bytes(remote_public_key)
            .unwrap_or_else(|error| unreachable!("fixed public key is valid: {error}"));
        diffie_hellman(secret.to_nonzero_scalar(), remote.as_affine())
            .raw_secret_bytes()
            .to_vec()
    }

    /// RFC 8291 section 5 fixes every input and therefore protects the complete encoding profile.
    #[test]
    fn standard_matches_rfc_8291_vector() {
        let sender = bytes(
            "04fe33f4ab0dea71914db55823f73b54948f41306d920732dbb9a59a53286482200e597a7b7bc260ba1c227998580992e93973002f3012a28ae8f06bbb78e5ec0f",
        );
        let receiver = bytes(
            "042571b2becdfde360551aaf1ed0f4cd366c11cebe555f89bcb7b186a53339173168ece2ebe018597bd30479b86e3c8f8eced577ca59187e9246990db682008b0e",
        );
        let shared = shared_secret(
            "c9f58f89813e9f8e872e71f42aa64e1757c9254dcc62b72ddc010bb4043ea11c",
            &receiver,
        );
        let encrypted = encrypt_standard(
            &receiver,
            &sender,
            &shared,
            &bytes("05305932a1c7eabe13b6cec9fda48882"),
            bytes("0c6bfaadad67958803092d454676f397")
                .try_into()
                .unwrap_or_else(|_| unreachable!("salt length")),
            b"When I grow up, I want to be a watermelon",
        )
        .unwrap_or_else(|error| unreachable!("vector encrypts: {error}"));
        assert_eq!(
            hex::encode(encrypted.body),
            "0c6bfaadad67958803092d454676f397000010004104fe33f4ab0dea71914db55823f73b54948f41306d920732dbb9a59a53286482200e597a7b7bc260ba1c227998580992e93973002f3012a28ae8f06bbb78e5ec0ff297de5b429bba7153d3a4ae0caa091fd425f3b4b5414add8ab37a19c1bbb05cf5cb5b2a2e0562d558635641ec52812c6c8ff42e95ccb86be7cd"
        );
    }

    /// The legacy draft vector protects the key schedule still used by non-standard clients.
    #[test]
    fn legacy_matches_web_push_draft_vector() {
        let receiver = bytes(
            "042124063ccbf19dc2fa88b643ba04e6dd8da7ea7ba2c8c62e0f77a943f4c2fa914f6d44116c9fd1c40341c6a440cab3e2140a60e4378a5da735972de078005105",
        );
        let sender = bytes(
            "04da110db6fce091a6f20e59e42171bab4aab17589d7522d7d71166152c4f3963b0989038d7b0811ce1aab161a4351bc06a917089e833e90eb5ad7568ff9ae8075",
        );
        let shared = shared_secret(
            "f455a5d79fd05100160da0f7937979d19059409e1abb6ec5d55e05d2e2d20ff3",
            &sender,
        );
        let encrypted = encrypt_legacy(
            &receiver,
            &sender,
            &shared,
            &bytes("476f6f20676f6f206727206a6f6f6221"),
            bytes("96781aadbc8a7cca22f59ef9c585e692")
                .try_into()
                .unwrap_or_else(|_| unreachable!("salt length")),
            b"I am the walrus",
        )
        .unwrap_or_else(|error| unreachable!("vector encrypts: {error}"));
        assert_eq!(
            hex::encode(encrypted.body),
            "ea7a80414304f2136ac39277925f1ca55549ca55ca62a64e7ac7991bc52e78aa40"
        );
    }

    #[test]
    fn enforces_encoding_payload_boundaries() {
        let receiver = bytes(
            "042571b2becdfde360551aaf1ed0f4cd366c11cebe555f89bcb7b186a53339173168ece2ebe018597bd30479b86e3c8f8eced577ca59187e9246990db682008b0e",
        );
        let sender = bytes(
            "04fe33f4ab0dea71914db55823f73b54948f41306d920732dbb9a59a53286482200e597a7b7bc260ba1c227998580992e93973002f3012a28ae8f06bbb78e5ec0f",
        );
        let shared = shared_secret(
            "c9f58f89813e9f8e872e71f42aa64e1757c9254dcc62b72ddc010bb4043ea11c",
            &receiver,
        );
        assert!(
            encrypt_standard(
                &receiver,
                &sender,
                &shared,
                &[1; 16],
                [2; 16],
                &vec![0; 3993]
            )
            .is_ok()
        );
        assert!(matches!(
            encrypt_standard(
                &receiver,
                &sender,
                &shared,
                &[1; 16],
                [2; 16],
                &vec![0; 3994]
            ),
            Err(WebPushError::PayloadTooLarge)
        ));
        assert!(
            encrypt_legacy(
                &receiver,
                &sender,
                &shared,
                &[1; 16],
                [2; 16],
                &vec![0; 4078]
            )
            .is_ok()
        );
        assert!(matches!(
            encrypt_legacy(
                &receiver,
                &sender,
                &shared,
                &[1; 16],
                [2; 16],
                &vec![0; 4079]
            ),
            Err(WebPushError::PayloadTooLarge)
        ));
    }
}
