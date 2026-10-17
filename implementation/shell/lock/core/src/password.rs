// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The password's record: an Argon2id hash of it in the PHC string format
//! (`$argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>`), which keeps the salt
//! and the parameters it was made with beside the hash. Stronger
//! parameters can come later without losing the passwords set before:
//! those are hashed again the next time they are typed
//! ([`Record::outdated`]).
//!
//! * **Hashing** takes Argon2's defaults, which are OWASP's choice for
//!   Argon2id: 19 MiB of memory and two passes, so that each guess costs
//!   whoever makes it what checking one costs the lock screen, a fraction
//!   of a second, and as much memory.
//! * **A record read back is untrusted** (it is a file): only Argon2id of
//!   version 1.3 is taken, salted with [`SALT_LEN`] bytes at least, with
//!   parameters within [`MAX_MEMORY_KIB`], [`MAX_PASSES`] and [`MAX_LANES`],
//!   so that no record can make checking a password take all of the
//!   machine's memory or time.
//! * **Checking** compares the hashes in constant time (`password-hash`'s
//!   `Output`), and costs the same whether the password is right or not.
//! * **New passwords** ([`acceptable`]) have at least `MIN_PASSWORD`
//!   characters and at most `MAX_PASSWORD` bytes (`vproto::lock`'s).

use alloc::string::{String, ToString};
use core::fmt;

use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::{Algorithm, Argon2, Params, Version};
use vproto::lock::{LockError, MAX_PASSWORD, MIN_PASSWORD};

/// Random bytes a password is salted with.
pub const SALT_LEN: usize = 16;
/// The longest record read (bytes): a record of the parameters taken is
/// some 100 bytes long.
pub const MAX_RECORD: usize = 256;
/// The most memory a record may make checking take (KiB), ...
pub const MAX_MEMORY_KIB: u32 = 256 * 1024;
/// ... the most passes over it, ...
pub const MAX_PASSES: u32 = 16;
/// ... and the most lanes.
pub const MAX_LANES: u32 = 4;
/// The hash's length (bytes).
const HASH_LEN: usize = 32;

/// Why a record was not taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordError {
    /// It is not a PHC string, or is longer than [`MAX_RECORD`] bytes.
    Malformed,
    /// It is a hash of another kind than Argon2id 1.3.
    Unsupported,
    /// Checking it would cost more than may be, or its salt is too short.
    Unsafe,
    /// The password could not be hashed (it can be, with the parameters
    /// taken).
    Hashing,
}

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            RecordError::Malformed => "the password's record is damaged",
            RecordError::Unsupported => "the password's record is of a hash other than Argon2id 1.3",
            RecordError::Unsafe => "the password's record asks for more than checking it may cost",
            RecordError::Hashing => "the password could not be hashed",
        })
    }
}

/// The record of a password (see the module's comment).
#[derive(Clone, PartialEq, Eq)]
pub struct Record(String);

impl fmt::Debug for Record {
    /// What hashes the password stays out of the log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Record(..)")
    }
}

impl Record {
    /// The record of `password`, salted with `salt` (random bytes, at least
    /// [`SALT_LEN`] of them).
    pub fn create(password: &[u8], salt: &[u8]) -> Result<Record, RecordError> {
        Record::create_with(password, salt, Params::DEFAULT)
    }

    /// The record of `password` made with `params` (cheaper ones, in tests).
    fn create_with(password: &[u8], salt: &[u8], params: Params) -> Result<Record, RecordError> {
        if salt.len() < SALT_LEN {
            return Err(RecordError::Unsafe);
        }
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let hash = argon2.hash_password_with_salt(password, salt).map_err(|_| RecordError::Hashing)?;
        Ok(Record(hash.to_string()))
    }

    /// A record as it was stored, checked (see the module's comment).
    pub fn parse(text: &str) -> Result<Record, RecordError> {
        let text = text.trim();
        if text.len() > MAX_RECORD {
            return Err(RecordError::Malformed);
        }
        let hash = PasswordHash::new(text).map_err(|_| RecordError::Malformed)?;
        checked(&hash)?;
        Ok(Record(text.to_string()))
    }

    /// Whether `password` is the one this is the record of.
    pub fn verify(&self, password: &[u8]) -> bool {
        let Ok(hash) = PasswordHash::new(&self.0) else { return false };
        if checked(&hash).is_err() {
            return false;
        }
        // The algorithm, version and parameters are the record's.
        Argon2::default().verify_password(password, &hash).is_ok()
    }

    /// Whether it was made with weaker parameters than a new one would be:
    /// the password is hashed anew the next time it is typed.
    pub fn outdated(&self) -> bool {
        let Ok(hash) = PasswordHash::new(&self.0) else { return true };
        let Ok(p) = checked(&hash) else { return true };
        let new = Params::DEFAULT;
        p.m_cost() < new.m_cost() || p.t_cost() < new.t_cost()
    }

    /// The record, to store.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The parameters of `hash`, if it is a record that is taken.
fn checked(hash: &PasswordHash) -> Result<Params, RecordError> {
    if hash.algorithm.as_str() != Algorithm::Argon2id.as_str() || hash.version != Some(Version::V0x13.into()) {
        return Err(RecordError::Unsupported);
    }
    let salt = hash.salt.as_ref().ok_or(RecordError::Malformed)?;
    if salt.len() < SALT_LEN {
        return Err(RecordError::Unsafe);
    }
    if hash.hash.as_ref().is_none_or(|h| h.len() != HASH_LEN) {
        return Err(RecordError::Malformed);
    }
    let p = Params::try_from(hash).map_err(|_| RecordError::Malformed)?;
    if p.m_cost() > MAX_MEMORY_KIB || p.t_cost() > MAX_PASSES || p.p_cost() > MAX_LANES {
        return Err(RecordError::Unsafe);
    }
    Ok(p)
}

/// Whether `password` may be the lock screen's new password.
pub fn acceptable(password: &str) -> Result<(), LockError> {
    if password.chars().count() < MIN_PASSWORD {
        Err(LockError::TooShort)
    } else if password.len() > MAX_PASSWORD {
        Err(LockError::TooLong)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SALT: &[u8; SALT_LEN] = b"0123456789abcdef";

    /// Cheap parameters: the tests check what records say, not their cost.
    fn cheap() -> Params {
        Params::new(64, 1, 1, Some(HASH_LEN)).unwrap()
    }

    fn record(password: &str) -> Record {
        Record::create_with(password.as_bytes(), SALT, cheap()).unwrap()
    }

    #[test]
    fn a_record_knows_its_password_alone() {
        let r = record("correct horse");
        assert!(r.verify(b"correct horse"));
        assert!(!r.verify(b"correct hors"));
        assert!(!r.verify(b"Correct horse"));
        assert!(!r.verify(b""));
        // Read back, it says the same.
        let back = Record::parse(r.as_str()).unwrap();
        assert!(back.verify(b"correct horse") && !back.verify(b"wrong"));
    }

    #[test]
    fn records_are_argon2id_with_their_salt_and_parameters() {
        // As a new record is made: Argon2's defaults (OWASP's choice).
        let r = Record::create(b"1234", SALT).unwrap();
        assert!(r.as_str().starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{}", r.as_str());
        assert!(r.verify(b"1234") && !r.outdated());
        // The same password with another salt has another hash.
        let other = Record::create_with(b"same", b"fedcba9876543210", cheap()).unwrap();
        assert_ne!(record("same").as_str(), other.as_str());
        // A weaker record is hashed anew when next typed.
        assert!(record("old").outdated());
        // What hashes the password stays out of the log.
        assert_eq!(std::format!("{r:?}"), "Record(..)");
    }

    #[test]
    fn records_read_back_are_checked() {
        let good = record("pass").as_str().to_string();
        assert_eq!(Record::parse(""), Err(RecordError::Malformed));
        assert_eq!(Record::parse("not a record"), Err(RecordError::Malformed));
        assert_eq!(Record::parse(&good[..good.len() - 5]), Err(RecordError::Malformed));
        assert_eq!(Record::parse(&std::format!("{good}{}", "A".repeat(MAX_RECORD))), Err(RecordError::Malformed));
        // Only Argon2id 1.3.
        assert_eq!(Record::parse(&good.replace("argon2id", "argon2i")), Err(RecordError::Unsupported));
        assert_eq!(Record::parse(&good.replace("argon2id", "argon2d")), Err(RecordError::Unsupported));
        assert_eq!(Record::parse(&good.replace("v=19", "v=16")), Err(RecordError::Unsupported));
        // Nothing that would take all of the machine's memory or time.
        assert_eq!(Record::parse(&good.replace("m=64", "m=4194304")), Err(RecordError::Unsafe));
        assert_eq!(Record::parse(&good.replace("t=1", "t=1000")), Err(RecordError::Unsafe));
        assert_eq!(Record::parse(&good.replace("m=64,t=1,p=1", "m=1024,t=1,p=64")), Err(RecordError::Unsafe));
        // A record whose parameters were changed knows no password.
        let changed = Record::parse(&good.replace("t=1", "t=2")).unwrap();
        assert!(!changed.verify(b"pass"));
        // White space around it, as an editor leaves it, is not part of it.
        assert!(Record::parse(&std::format!("{good}\n")).unwrap().verify(b"pass"));
    }

    #[test]
    fn short_salts_are_refused() {
        assert_eq!(Record::create_with(b"pass", b"salt", cheap()), Err(RecordError::Unsafe));
        // Eight bytes of salt ("saltsalt"), which PHC strings may have, are
        // not taken.
        let short = "$argon2id$v=19$m=64,t=1,p=1$c2FsdHNhbHQ$vgKvRqbW5RUmsZlFLtCxKXCYhrjhVEV4bE3DzAqKaog";
        assert_eq!(Record::parse(short), Err(RecordError::Unsafe));
    }

    #[test]
    fn new_passwords_are_long_enough_and_not_too_long() {
        assert_eq!(acceptable("abc"), Err(LockError::TooShort));
        assert_eq!(acceptable(""), Err(LockError::TooShort));
        assert_eq!(acceptable("1234"), Ok(()));
        // Characters, not bytes: four letters with accents are four.
        assert_eq!(acceptable("éèêë"), Ok(()));
        assert_eq!(acceptable(&"a".repeat(MAX_PASSWORD)), Ok(()));
        assert_eq!(acceptable(&"a".repeat(MAX_PASSWORD + 1)), Err(LockError::TooLong));
    }
}
