//! A software passkey for tests: what a browser and its authenticator
//! answer to the server's WebAuthn options. P-256 keys, no attestation,
//! user presence and verification always set.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use p256::ecdsa::{signature::Signer, DerSignature, SigningKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub struct SoftPasskey {
    key: SigningKey,
    pub credential_id: Vec<u8>,
    user_handle: Option<String>,
    counter: u32,
    /// The origin the "browser" reports in its client data.
    pub origin: String,
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// CBOR header of a byte string.
fn cbor_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut out = match bytes.len() {
        len if len < 24 => vec![0x40 + len as u8],
        len if len < 256 => vec![0x58, len as u8],
        len => vec![0x59, (len >> 8) as u8, len as u8],
    };
    out.extend_from_slice(bytes);
    out
}

impl SoftPasskey {
    pub fn new(origin: &str) -> Self {
        let key = loop {
            if let Ok(key) = SigningKey::from_slice(&rand::random::<[u8; 32]>()) {
                break key;
            }
        };
        Self {
            key,
            credential_id: rand::random::<[u8; 16]>().to_vec(),
            user_handle: None,
            counter: 0,
            origin: origin.to_owned(),
        }
    }

    fn client_data(&self, kind: &str, options: &Value) -> Vec<u8> {
        json!({
            "type": kind,
            "challenge": options["publicKey"]["challenge"],
            "origin": self.origin,
            "crossOrigin": false,
        })
        .to_string()
        .into_bytes()
    }

    /// COSE_Key of the public key: EC2, ES256, P-256.
    fn cose_key(&self) -> Vec<u8> {
        let point = self.key.verifying_key().to_encoded_point(false);
        let mut out = vec![0xa5, 0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21];
        out.extend(cbor_bytes(point.x().unwrap()));
        out.push(0x22);
        out.extend(cbor_bytes(point.y().unwrap()));
        out
    }

    /// Answers `navigator.credentials.create()`.
    pub fn register(&mut self, options: &Value) -> Value {
        let public_key = &options["publicKey"];
        self.user_handle = Some(public_key["user"]["id"].as_str().unwrap().to_owned());
        let rp_id = public_key["rp"]["id"].as_str().unwrap();
        let mut auth_data = Sha256::digest(rp_id.as_bytes()).to_vec();
        auth_data.push(0x01 | 0x04 | 0x40); // user present, verified, credential data
        auth_data.extend(self.counter.to_be_bytes());
        auth_data.extend([0; 16]); // AAGUID
        auth_data.extend((self.credential_id.len() as u16).to_be_bytes());
        auth_data.extend(&self.credential_id);
        auth_data.extend(self.cose_key());

        let mut attestation = vec![0xa3];
        attestation.extend(b"\x63fmt\x64none\x67attStmt\xa0\x68authData");
        attestation.extend(cbor_bytes(&auth_data));
        json!({
            "id": b64(&self.credential_id),
            "rawId": b64(&self.credential_id),
            "type": "public-key",
            "response": {
                "attestationObject": b64(&attestation),
                "clientDataJSON": b64(&self.client_data("webauthn.create", options)),
            },
        })
    }

    /// Answers `navigator.credentials.get()`.
    pub fn sign_in(&mut self, options: &Value) -> Value {
        let rp_id = options["publicKey"]["rpId"].as_str().unwrap();
        self.counter += 1;
        let mut auth_data = Sha256::digest(rp_id.as_bytes()).to_vec();
        auth_data.push(0x01 | 0x04);
        auth_data.extend(self.counter.to_be_bytes());
        let client_data = self.client_data("webauthn.get", options);
        let mut signed = auth_data.clone();
        signed.extend(Sha256::digest(&client_data));
        let signature: DerSignature = self.key.sign(&signed);
        json!({
            "id": b64(&self.credential_id),
            "rawId": b64(&self.credential_id),
            "type": "public-key",
            "response": {
                "authenticatorData": b64(&auth_data),
                "clientDataJSON": b64(&client_data),
                "signature": b64(signature.as_bytes()),
                "userHandle": self.user_handle,
            },
        })
    }
}
