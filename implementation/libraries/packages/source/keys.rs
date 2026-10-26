// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The keys that sign repositories' indexes: Ed25519 (RFC 8032), whose
//! signatures are deterministic (the same index signs the same every time,
//! so that builds of the system image are reproducible), small, quick to
//! check, and have no parameters to choose wrong.
//!
//! * A public key is written `ed25519:` and its 32 bytes in Base64
//!   (`ed25519:9z3pJ...=`), as a repository's configuration names it.
//! * Its *id* is the SHA-256 of its 32 bytes, by which an index says which
//!   key made each of its signatures; its *fingerprint*, the id's first 16
//!   hexadecimal digits, is how people tell keys apart.
//! * A secret key is a file of one line, `ed25519-secret:` and the key's
//!   32-byte seed in Base64, after comments. Whoever has it can sign
//!   indexes that systems trusting its public key accept: it stays with
//!   whoever publishes the repository, never on the systems that use it.
//!
//! Signatures are checked strictly (`verify_strict`): a signature that
//! another one could be forged from, and keys of small order, which a
//! signature of any message could check against, are refused.

use alloc::string::String;
use core::fmt;

use base64ct::{Base64, Encoding};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use zeroize::Zeroizing;

use crate::Digest;

/// What a public key's text begins with.
const PUBLIC_PREFIX: &str = "ed25519:";
/// What a secret key's line begins with.
const SECRET_PREFIX: &str = "ed25519-secret:";

/// The bytes of a signature.
pub const SIGNATURE_LEN: usize = 64;

/// Why a key's text is not a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadKey(pub String);

impl fmt::Display for BadKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A key that checks signatures.
#[derive(Clone, PartialEq, Eq)]
pub struct PublicKey {
    key: VerifyingKey,
    id: Digest,
}

impl PublicKey {
    /// Reads `ed25519:` and 32 bytes in Base64.
    pub fn parse(text: &str) -> Result<PublicKey, BadKey> {
        let bad = || {
            BadKey(alloc::format!(
                "'{}' is not a public key: one is written ed25519: and 32 bytes in Base64",
                text.chars().take(80).collect::<String>()
            ))
        };
        let encoded = text.trim().strip_prefix(PUBLIC_PREFIX).ok_or_else(bad)?;
        let mut bytes = [0u8; 32];
        let decoded = Base64::decode(encoded, &mut bytes).map_err(|_| bad())?;
        if decoded.len() != 32 {
            return Err(bad());
        }
        PublicKey::from_bytes(&bytes)
    }

    /// The key whose 32 bytes are `bytes`, unless it is no key or one of
    /// small order.
    pub fn from_bytes(bytes: &[u8; 32]) -> Result<PublicKey, BadKey> {
        let key =
            VerifyingKey::from_bytes(bytes).map_err(|_| BadKey(String::from("the key is not a point of the curve")))?;
        if key.is_weak() {
            return Err(BadKey(String::from("the key is of small order: any signature would check against it")));
        }
        Ok(PublicKey { key, id: Digest::of(bytes) })
    }

    /// The key's 32 bytes.
    pub fn bytes(&self) -> [u8; 32] {
        self.key.to_bytes()
    }

    /// The key's id: the SHA-256 of its bytes.
    pub fn id(&self) -> Digest {
        self.id
    }

    /// The key's fingerprint, by which people tell it from others.
    pub fn fingerprint(&self) -> String {
        self.id.short(16)
    }

    /// Whether `signature` is this key's of `message`.
    pub fn verify(&self, message: &[u8], signature: &[u8; SIGNATURE_LEN]) -> bool {
        self.key.verify_strict(message, &Signature::from_bytes(signature)).is_ok()
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{PUBLIC_PREFIX}{}", Base64::encode_string(&self.key.to_bytes()))
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", self.fingerprint())
    }
}

impl serde::Serialize for PublicKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for PublicKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<PublicKey, D::Error> {
        let text: &str = serde::Deserialize::deserialize(d)?;
        PublicKey::parse(text).map_err(serde::de::Error::custom)
    }
}

/// A key that signs. Its secret is wiped from memory when it is dropped.
pub struct SecretKey {
    key: SigningKey,
}

impl SecretKey {
    /// The key made from a 32-byte seed, which must be random: the
    /// caller's from the system's generator.
    pub fn from_seed(seed: &[u8; 32]) -> SecretKey {
        SecretKey { key: SigningKey::from_bytes(seed) }
    }

    /// Reads a secret key's file: comments (`#`), then `ed25519-secret:`
    /// and the seed in Base64.
    pub fn parse(text: &str) -> Result<SecretKey, BadKey> {
        let bad = || BadKey(String::from("not a secret key: its line is ed25519-secret: and 32 bytes in Base64"));
        let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'));
        let (Some(line), None) = (lines.next(), lines.next()) else { return Err(bad()) };
        let encoded = line.strip_prefix(SECRET_PREFIX).ok_or_else(bad)?;
        let mut seed = Zeroizing::new([0u8; 32]);
        let decoded = Base64::decode(encoded, &mut *seed).map_err(|_| bad())?.len();
        if decoded != 32 {
            return Err(bad());
        }
        Ok(SecretKey::from_seed(&seed))
    }

    /// The key's file, as [`SecretKey::parse`] reads it.
    pub fn to_text(&self) -> Zeroizing<String> {
        let seed = Zeroizing::new(self.key.to_bytes());
        let encoded = Zeroizing::new(Base64::encode_string(&*seed));
        Zeroizing::new(alloc::format!(
            "# A secret key that signs Veda package repositories (public key {}).\n\
             # Whoever has it can sign what systems that trust the public key install: keep it secret.\n\
             {SECRET_PREFIX}{}\n",
            self.public(),
            encoded.as_str()
        ))
    }

    /// The key that checks this one's signatures.
    pub fn public(&self) -> PublicKey {
        let key = self.key.verifying_key();
        PublicKey { key, id: Digest::of(&key.to_bytes()) }
    }

    /// Signs `message`.
    pub fn sign(&self, message: &[u8]) -> [u8; SIGNATURE_LEN] {
        self.key.sign(message).to_bytes()
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretKey(of {})", self.public().fingerprint())
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::*;

    fn key(n: u8) -> SecretKey {
        SecretKey::from_seed(&[n; 32])
    }

    #[test]
    fn keys_sign_and_check() {
        let (a, b) = (key(1), key(2));
        let signature = a.sign(b"an index");
        assert!(a.public().verify(b"an index", &signature));
        assert!(!a.public().verify(b"another index", &signature));
        assert!(!b.public().verify(b"an index", &signature));
        // Ed25519's signatures are the same every time.
        assert_eq!(a.sign(b"an index"), signature);
        let mut bent = signature;
        bent[10] ^= 1;
        assert!(!a.public().verify(b"an index", &bent));
    }

    #[test]
    fn public_keys_are_written_in_base64() {
        let public = key(7).public();
        let text = public.to_string();
        assert!(text.starts_with("ed25519:") && text.len() == 8 + 44, "{text}");
        let back = PublicKey::parse(&text).unwrap();
        assert_eq!(back, public);
        assert_eq!(back.fingerprint().len(), 16);
        assert_eq!(back.id(), Digest::of(&public.bytes()));
        for bad in ["", "ed25519:", "rsa:AAAA", "ed25519:AAAA", &text[..text.len() - 4], &text.replace("ed25519:", "")]
        {
            assert!(PublicKey::parse(bad).is_err(), "{bad:?}");
        }
        // The identity point is of small order: refused.
        let mut identity = [0u8; 32];
        identity[0] = 1;
        assert!(PublicKey::from_bytes(&identity).unwrap_err().to_string().contains("small order"));
    }

    #[test]
    fn secret_keys_are_files_of_one_line() {
        let secret = key(9);
        let text = secret.to_text();
        assert!(text.contains(&secret.public().to_string()));
        let back = SecretKey::parse(&text).unwrap();
        assert_eq!(back.public(), secret.public());
        assert!(SecretKey::parse("ed25519-secret:AAAA").is_err());
        assert!(SecretKey::parse(&alloc::format!("{}\n{}", text.as_str(), text.as_str())).is_err());
        assert!(SecretKey::parse(&secret.public().to_string()).is_err());
        assert!(!alloc::format!("{secret:?}").contains("secret:"));
    }
}
