//! Passkeys (WebAuthn) for signing in to the UI.
//!
//! Hub is single-user, so every passkey is the owner's and sign-in is
//! "usernameless": the browser offers whichever Hub passkey it has. Only
//! ES256 (ECDSA P-256) keys are accepted; every passkey provider (iCloud
//! Keychain, Google Password Manager, Windows Hello, password managers)
//! supports them. Attestation isn't requested: Hub doesn't care which
//! device made the key, only that the same key signs in later.
//!
//! Verification follows the WebAuthn Level 2 steps that matter without
//! attestation: client data type, challenge and origin; the RP ID hash; user
//! presence and verification flags; the credential's public key on
//! registration; the signature over `authenticatorData || SHA-256(clientDataJSON)`
//! and the sign counter on sign-in.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ciborium::Value as Cbor;
use p256::ecdsa::signature::Verifier as _;
use p256::ecdsa::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const FLAG_UP: u8 = 0x01;
const FLAG_UV: u8 = 0x04;
const FLAG_AT: u8 = 0x40;
/// COSE algorithm id for ES256.
pub const ES256: i64 = -7;

/// Why a registration or sign-in was refused. Logged; the user gets a
/// plain message.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refused {
    #[error("malformed {0}")]
    Malformed(&'static str),
    #[error("client data type is {0:?}")]
    WrongType(String),
    #[error("challenge doesn't match")]
    Challenge,
    #[error("origin {0:?} isn't Hub's")]
    Origin(String),
    #[error("signed for another site")]
    RpId,
    #[error("the user wasn't present or verified")]
    Flags,
    #[error("only ES256 (P-256) passkeys are supported")]
    Algorithm,
    #[error("signature doesn't verify")]
    Signature,
    #[error("sign counter went backwards (a cloned authenticator?)")]
    Counter,
}

/// Who Hub is, to WebAuthn: the RP ID is the host of `public_origin`.
#[derive(Debug, Clone)]
pub struct RelyingParty {
    pub id: String,
    pub origin: String,
}

impl RelyingParty {
    pub fn from_origin(public_origin: &str) -> Self {
        let origin = public_origin.trim_end_matches('/').to_string();
        let host = origin
            .split("://")
            .nth(1)
            .unwrap_or(&origin)
            .split(['/', ':'])
            .next()
            .unwrap_or("")
            .to_string();
        Self { id: host, origin }
    }
}

pub fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn unb64(s: &str, what: &'static str) -> Result<Vec<u8>, Refused> {
    URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .map_err(|_| Refused::Malformed(what))
}

#[derive(Deserialize)]
struct ClientData {
    #[serde(rename = "type")]
    kind: String,
    challenge: String,
    origin: String,
}

fn check_client_data(
    rp: &RelyingParty,
    json: &[u8],
    kind: &str,
    challenge: &str,
) -> Result<(), Refused> {
    let data: ClientData =
        serde_json::from_slice(json).map_err(|_| Refused::Malformed("clientDataJSON"))?;
    if data.kind != kind {
        return Err(Refused::WrongType(data.kind));
    }
    if data.challenge.trim_end_matches('=') != challenge {
        return Err(Refused::Challenge);
    }
    if data.origin != rp.origin {
        return Err(Refused::Origin(data.origin));
    }
    Ok(())
}

/// The fixed part of authenticator data: RP ID hash, flags, sign counter.
fn check_auth_data(rp: &RelyingParty, data: &[u8]) -> Result<(u8, u32), Refused> {
    if data.len() < 37 {
        return Err(Refused::Malformed("authenticatorData"));
    }
    if data[..32] != Sha256::digest(rp.id.as_bytes())[..] {
        return Err(Refused::RpId);
    }
    let flags = data[32];
    if flags & FLAG_UP == 0 || flags & FLAG_UV == 0 {
        return Err(Refused::Flags);
    }
    let count = u32::from_be_bytes(data[33..37].try_into().expect("4 bytes"));
    Ok((flags, count))
}

/// A verified new passkey.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCredential {
    pub id: Vec<u8>,
    /// SEC1 uncompressed point.
    pub public_key: Vec<u8>,
    pub sign_count: u32,
}

/// Check a `navigator.credentials.create()` response.
pub fn verify_registration(
    rp: &RelyingParty,
    challenge: &str,
    client_data_json: &[u8],
    attestation_object: &[u8],
) -> Result<NewCredential, Refused> {
    check_client_data(rp, client_data_json, "webauthn.create", challenge)?;
    let att: Cbor = ciborium::from_reader(attestation_object)
        .map_err(|_| Refused::Malformed("attestationObject"))?;
    let auth_data = map_get(&att, |k| k.as_text() == Some("authData"))
        .and_then(Cbor::as_bytes)
        .ok_or(Refused::Malformed("attestationObject"))?;
    let (flags, sign_count) = check_auth_data(rp, auth_data)?;
    if flags & FLAG_AT == 0 || auth_data.len() < 55 {
        return Err(Refused::Malformed("attested credential data"));
    }
    let id_len = u16::from_be_bytes([auth_data[53], auth_data[54]]) as usize;
    let id_end = 55 + id_len;
    if auth_data.len() < id_end || id_len == 0 {
        return Err(Refused::Malformed("credential id"));
    }
    let id = auth_data[55..id_end].to_vec();
    // The COSE key is the next CBOR value (extensions may follow it).
    let key: Cbor = ciborium::from_reader(&auth_data[id_end..])
        .map_err(|_| Refused::Malformed("credential public key"))?;
    let int = |n: i64| move |k: &Cbor| k.as_integer().map(i128::from) == Some(n.into());
    let alg = map_get(&key, int(3))
        .and_then(Cbor::as_integer)
        .map(i128::from);
    let kty = map_get(&key, int(1))
        .and_then(Cbor::as_integer)
        .map(i128::from);
    let crv = map_get(&key, int(-1))
        .and_then(Cbor::as_integer)
        .map(i128::from);
    if alg != Some(ES256.into()) || kty != Some(2) || crv != Some(1) {
        return Err(Refused::Algorithm);
    }
    let coord = |n: i64| {
        map_get(&key, int(n))
            .and_then(Cbor::as_bytes)
            .filter(|b| b.len() == 32)
            .ok_or(Refused::Malformed("credential public key"))
    };
    let mut public_key = vec![0x04];
    public_key.extend_from_slice(coord(-2)?);
    public_key.extend_from_slice(coord(-3)?);
    VerifyingKey::from_sec1_bytes(&public_key)
        .map_err(|_| Refused::Malformed("credential public key"))?;
    Ok(NewCredential {
        id,
        public_key,
        sign_count,
    })
}

/// Check a `navigator.credentials.get()` response against a stored passkey.
/// Returns the new sign counter.
pub fn verify_assertion(
    rp: &RelyingParty,
    challenge: &str,
    client_data_json: &[u8],
    authenticator_data: &[u8],
    signature: &[u8],
    public_key: &[u8],
    stored_count: u32,
) -> Result<u32, Refused> {
    check_client_data(rp, client_data_json, "webauthn.get", challenge)?;
    let (_, count) = check_auth_data(rp, authenticator_data)?;
    let key = VerifyingKey::from_sec1_bytes(public_key).map_err(|_| Refused::Signature)?;
    let sig = Signature::from_der(signature).map_err(|_| Refused::Malformed("signature"))?;
    let mut signed = authenticator_data.to_vec();
    signed.extend_from_slice(&Sha256::digest(client_data_json));
    key.verify(&signed, &sig).map_err(|_| Refused::Signature)?;
    // Synced passkeys always send 0; a counter that's in use must go up.
    if (count != 0 || stored_count != 0) && count <= stored_count {
        return Err(Refused::Counter);
    }
    Ok(count)
}

fn map_get(map: &Cbor, key: impl Fn(&Cbor) -> bool) -> Option<&Cbor> {
    map.as_map()?.iter().find(|(k, _)| key(k)).map(|(_, v)| v)
}

// ---------- Options sent to the browser ----------

/// The fixed user handle: there's one user.
pub const USER_HANDLE: &[u8] = b"hub-owner";

#[derive(Debug, Serialize)]
pub struct CreationOptions {
    pub challenge: String,
    pub rp: RpInfo,
    pub user: UserInfo,
    #[serde(rename = "pubKeyCredParams")]
    pub params: Vec<Param>,
    #[serde(rename = "authenticatorSelection")]
    pub selection: Selection,
    pub attestation: &'static str,
    #[serde(rename = "excludeCredentials")]
    pub exclude: Vec<Descriptor>,
    pub timeout: u32,
}

#[derive(Debug, Serialize)]
pub struct RpInfo {
    pub id: String,
    pub name: &'static str,
}

#[derive(Debug, Serialize)]
pub struct UserInfo {
    pub id: String,
    pub name: &'static str,
    #[serde(rename = "displayName")]
    pub display_name: &'static str,
}

#[derive(Debug, Serialize)]
pub struct Param {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub alg: i64,
}

#[derive(Debug, Serialize)]
pub struct Selection {
    #[serde(rename = "residentKey")]
    pub resident_key: &'static str,
    #[serde(rename = "userVerification")]
    pub user_verification: &'static str,
}

#[derive(Debug, Serialize)]
pub struct Descriptor {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub id: String,
}

#[derive(Debug, Serialize)]
pub struct RequestOptions {
    pub challenge: String,
    #[serde(rename = "rpId")]
    pub rp_id: String,
    #[serde(rename = "userVerification")]
    pub user_verification: &'static str,
    pub timeout: u32,
}

pub fn creation_options(
    rp: &RelyingParty,
    challenge: String,
    existing: Vec<String>,
) -> CreationOptions {
    CreationOptions {
        challenge,
        rp: RpInfo {
            id: rp.id.clone(),
            name: "Hub",
        },
        user: UserInfo {
            id: b64(USER_HANDLE),
            name: "hub",
            display_name: "Hub",
        },
        params: vec![Param {
            kind: "public-key",
            alg: ES256,
        }],
        selection: Selection {
            resident_key: "required",
            user_verification: "required",
        },
        attestation: "none",
        exclude: existing
            .into_iter()
            .map(|id| Descriptor {
                kind: "public-key",
                id,
            })
            .collect(),
        timeout: 120_000,
    }
}

pub fn request_options(rp: &RelyingParty, challenge: String) -> RequestOptions {
    RequestOptions {
        challenge,
        rp_id: rp.id.clone(),
        user_verification: "required",
        timeout: 120_000,
    }
}

#[cfg(test)]
mod tests;
