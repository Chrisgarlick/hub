//! A simulated authenticator: a real P-256 key producing real CBOR and
//! signatures, so the checks run against what a browser would send.

use ciborium::Value as Cbor;
use p256::ecdsa::signature::Signer as _;
use p256::ecdsa::{Signature, SigningKey};
use sha2::{Digest, Sha256};

use super::*;

const ORIGIN: &str = "https://hub.kritano.com";

fn rp() -> RelyingParty {
    RelyingParty::from_origin(ORIGIN)
}

fn key() -> SigningKey {
    SigningKey::from_slice(&[7u8; 32]).unwrap()
}

fn cose(key: &SigningKey) -> Vec<u8> {
    let point = key.verifying_key().to_sec1_point(false);
    let xy = point.as_bytes(); // 0x04 || x || y
    let map = Cbor::Map(vec![
        (Cbor::from(1), Cbor::from(2)),
        (Cbor::from(3), Cbor::from(ES256)),
        (Cbor::from(-1), Cbor::from(1)),
        (Cbor::from(-2), Cbor::Bytes(xy[1..33].to_vec())),
        (Cbor::from(-3), Cbor::Bytes(xy[33..65].to_vec())),
    ]);
    let mut out = Vec::new();
    ciborium::into_writer(&map, &mut out).unwrap();
    out
}

fn auth_data(rp_id: &str, flags: u8, count: u32, credential: Option<(&[u8], &[u8])>) -> Vec<u8> {
    let mut d = Sha256::digest(rp_id.as_bytes()).to_vec();
    d.push(flags);
    d.extend_from_slice(&count.to_be_bytes());
    if let Some((id, cose)) = credential {
        d.extend_from_slice(&[0u8; 16]); // AAGUID
        d.extend_from_slice(&(id.len() as u16).to_be_bytes());
        d.extend_from_slice(id);
        d.extend_from_slice(cose);
    }
    d
}

fn attestation(auth_data: Vec<u8>) -> Vec<u8> {
    let map = Cbor::Map(vec![
        (Cbor::Text("fmt".into()), Cbor::Text("none".into())),
        (Cbor::Text("attStmt".into()), Cbor::Map(vec![])),
        (Cbor::Text("authData".into()), Cbor::Bytes(auth_data)),
    ]);
    let mut out = Vec::new();
    ciborium::into_writer(&map, &mut out).unwrap();
    out
}

fn client_data(kind: &str, challenge: &str, origin: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "type": kind, "challenge": challenge, "origin": origin, "crossOrigin": false }))
        .unwrap()
}

const UP_UV: u8 = FLAG_UP | FLAG_UV;

fn register() -> NewCredential {
    let k = key();
    let data = auth_data(
        "hub.kritano.com",
        UP_UV | FLAG_AT,
        0,
        Some((b"cred-1", &cose(&k))),
    );
    verify_registration(
        &rp(),
        "chal",
        &client_data("webauthn.create", "chal", ORIGIN),
        &attestation(data),
    )
    .unwrap()
}

fn sign(k: &SigningKey, data: &[u8], client: &[u8]) -> Vec<u8> {
    let mut msg = data.to_vec();
    msg.extend_from_slice(&Sha256::digest(client));
    let sig: Signature = k.sign(&msg);
    sig.to_der().as_bytes().to_vec()
}

#[test]
fn the_rp_id_is_the_host() {
    assert_eq!(rp().id, "hub.kritano.com");
    let local = RelyingParty::from_origin("http://localhost:7070/");
    assert_eq!(
        (local.id.as_str(), local.origin.as_str()),
        ("localhost", "http://localhost:7070")
    );
}

#[test]
fn a_passkey_registers_and_signs_in() {
    let cred = register();
    assert_eq!(cred.id, b"cred-1");
    assert_eq!(cred.public_key.len(), 65);

    let data = auth_data("hub.kritano.com", UP_UV, 0, None);
    let client = client_data("webauthn.get", "login-chal", ORIGIN);
    let sig = sign(&key(), &data, &client);
    let count = verify_assertion(
        &rp(),
        "login-chal",
        &client,
        &data,
        &sig,
        &cred.public_key,
        0,
    )
    .unwrap();
    assert_eq!(count, 0, "synced passkeys keep a zero counter");
}

#[test]
fn registration_checks() {
    let k = key();
    let good = auth_data(
        "hub.kritano.com",
        UP_UV | FLAG_AT,
        0,
        Some((b"c", &cose(&k))),
    );
    let client = client_data("webauthn.create", "chal", ORIGIN);
    let try_reg = |client: &[u8], data: Vec<u8>| {
        verify_registration(&rp(), "chal", client, &attestation(data))
    };

    assert_eq!(
        try_reg(&client_data("webauthn.get", "chal", ORIGIN), good.clone()),
        Err(Refused::WrongType("webauthn.get".into()))
    );
    assert_eq!(
        try_reg(
            &client_data("webauthn.create", "other", ORIGIN),
            good.clone()
        ),
        Err(Refused::Challenge)
    );
    assert_eq!(
        try_reg(
            &client_data("webauthn.create", "chal", "https://evil.example"),
            good.clone()
        ),
        Err(Refused::Origin("https://evil.example".into()))
    );
    let elsewhere = auth_data("evil.example", UP_UV | FLAG_AT, 0, Some((b"c", &cose(&k))));
    assert_eq!(try_reg(&client, elsewhere), Err(Refused::RpId));
    let unverified = auth_data(
        "hub.kritano.com",
        FLAG_UP | FLAG_AT,
        0,
        Some((b"c", &cose(&k))),
    );
    assert_eq!(try_reg(&client, unverified), Err(Refused::Flags));

    // A key that isn't ES256.
    let rsa = Cbor::Map(vec![
        (Cbor::from(1), Cbor::from(3)),
        (Cbor::from(3), Cbor::from(-257)),
    ]);
    let mut rsa_bytes = Vec::new();
    ciborium::into_writer(&rsa, &mut rsa_bytes).unwrap();
    let data = auth_data(
        "hub.kritano.com",
        UP_UV | FLAG_AT,
        0,
        Some((b"c", &rsa_bytes)),
    );
    assert_eq!(try_reg(&client, data), Err(Refused::Algorithm));

    assert!(matches!(
        verify_registration(&rp(), "chal", &client, b"not cbor"),
        Err(Refused::Malformed(_))
    ));
}

#[test]
fn sign_in_checks() {
    let cred = register();
    let data = auth_data("hub.kritano.com", UP_UV, 5, None);
    let client = client_data("webauthn.get", "c", ORIGIN);
    let sig = sign(&key(), &data, &client);
    let check = |data: &[u8], client: &[u8], sig: &[u8], stored: u32| {
        verify_assertion(&rp(), "c", client, data, sig, &cred.public_key, stored)
    };

    assert_eq!(check(&data, &client, &sig, 4), Ok(5));
    assert_eq!(
        check(&data, &client, &sig, 5),
        Err(Refused::Counter),
        "a counter that doesn't go up"
    );

    // Another key's signature, a tampered message, a replayed challenge.
    let other = SigningKey::from_slice(&[9u8; 32]).unwrap();
    assert_eq!(
        check(&data, &client, &sign(&other, &data, &client), 0),
        Err(Refused::Signature)
    );
    let tampered = client_data("webauthn.get", "c", ORIGIN)
        .iter()
        .copied()
        .chain(*b" ")
        .collect::<Vec<u8>>();
    assert_eq!(check(&data, &tampered, &sig, 0), Err(Refused::Signature));
    let replay = client_data("webauthn.get", "old", ORIGIN);
    assert_eq!(
        check(&data, &replay, &sign(&key(), &data, &replay), 0),
        Err(Refused::Challenge)
    );

    let unverified = auth_data("hub.kritano.com", FLAG_UP, 6, None);
    assert_eq!(
        check(&unverified, &client, &sign(&key(), &unverified, &client), 0),
        Err(Refused::Flags)
    );
}
