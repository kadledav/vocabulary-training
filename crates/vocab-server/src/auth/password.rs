//! The owner's password hash: argon2id PHC strings (design §9.1).
//!
//! New passwords are hashed with the OWASP minimum parameters for argon2id
//! (m = 19 MiB, t = 2, p = 1). A stored hash is only accepted when it is
//! argon2id v19 with at least those parameters and at most what fits the
//! container's memory limit, so a weak or absurd hash is an error rather than
//! a silent risk.

use argon2::password_hash::phc::PasswordHash;
use argon2::{Algorithm, Argon2, Params, PasswordHasher, PasswordVerifier, Version};
use sha2::{Digest, Sha256};

/// Parameters of new hashes: the OWASP minimum for argon2id.
const M_COST_KIB: u32 = 19 * 1024;
const T_COST: u32 = 2;
const P_COST: u32 = 1;
/// Upper bounds of an accepted hash (memory limit of the container: 256 MB).
const MAX_M_COST_KIB: u32 = 128 * 1024;
const MAX_T_COST: u32 = 16;
const MAX_P_COST: u32 = 8;
/// Salt length of new hashes, in bytes.
const SALT_LEN: usize = 16;

/// Longest password setup, login and a password change accept, in bytes.
pub const MAX_PASSWORD_BYTES: usize = 1024;
/// Shortest new password, in characters.
pub const MIN_PASSWORD_CHARS: usize = 12;

/// Why a stored hash or a new password is refused.
#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    #[error("is not an argon2 PHC string")]
    Phc,
    #[error("must be argon2id version 19, got {0}")]
    Algorithm(String),
    #[error(
        "parameters m={m}, t={t}, p={p} are outside the accepted range \
         (m {M_COST_KIB}-{MAX_M_COST_KIB} KiB, t {T_COST}-{MAX_T_COST}, p {P_COST}-{MAX_P_COST})"
    )]
    Params { m: u32, t: u32, p: u32 },
    #[error("has no salt or no hash value")]
    Incomplete,
    #[error(
        "the password must be at least {MIN_PASSWORD_CHARS} characters \
         (and at most {MAX_PASSWORD_BYTES} bytes) long"
    )]
    Length,
    #[error("no random numbers available: {0}")]
    Random(String),
    #[error("hashing failed: {0}")]
    Hash(String),
}

/// A validated argon2id PHC string. `Debug` never shows it.
#[derive(Clone)]
pub struct PasswordHashConfig {
    phc: String,
    fingerprint: String,
}

impl std::fmt::Debug for PasswordHashConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PasswordHashConfig(***)")
    }
}

impl PasswordHashConfig {
    /// Parse and check a stored PHC string.
    pub fn from_phc(phc: &str) -> Result<Self, PasswordError> {
        let parsed = PasswordHash::new(phc).map_err(|_| PasswordError::Phc)?;
        if parsed.algorithm.as_str() != Algorithm::Argon2id.as_str() || parsed.version != Some(19) {
            return Err(PasswordError::Algorithm(format!(
                "{} v{}",
                parsed.algorithm.as_str(),
                parsed
                    .version
                    .map_or_else(|| "?".to_owned(), |v| v.to_string())
            )));
        }
        let params = Params::try_from(&parsed).map_err(|_| PasswordError::Phc)?;
        let (m, t, p) = (params.m_cost(), params.t_cost(), params.p_cost());
        if !(M_COST_KIB..=MAX_M_COST_KIB).contains(&m)
            || !(T_COST..=MAX_T_COST).contains(&t)
            || !(P_COST..=MAX_P_COST).contains(&p)
        {
            return Err(PasswordError::Params { m, t, p });
        }
        if parsed.salt.is_none() || parsed.hash.is_none() {
            return Err(PasswordError::Incomplete);
        }
        let fingerprint = hex::encode(Sha256::digest(phc.as_bytes()));
        Ok(Self {
            phc: phc.to_owned(),
            fingerprint,
        })
    }

    /// The PHC string, as stored.
    pub fn phc(&self) -> &str {
        &self.phc
    }

    /// SHA-256 of the PHC string: stored with every session, so a new
    /// password (a new hash) ends all sessions.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Check `password` (CPU and memory heavy: call it off the async runtime).
    pub fn verify(&self, password: &str) -> bool {
        if password.len() > MAX_PASSWORD_BYTES {
            return false;
        }
        let Ok(parsed) = PasswordHash::new(&self.phc) else {
            return false;
        };
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    }
}

/// Is `password` acceptable as a new password (length only)?
pub fn check_new(password: &str) -> Result<(), PasswordError> {
    if password.chars().count() < MIN_PASSWORD_CHARS || password.len() > MAX_PASSWORD_BYTES {
        return Err(PasswordError::Length);
    }
    Ok(())
}

/// Hash a new `password` with a fresh salt (CPU and memory heavy: call it
/// off the async runtime).
pub fn hash_password(password: &str) -> Result<PasswordHashConfig, PasswordError> {
    check_new(password)?;
    let mut salt = [0u8; SALT_LEN];
    getrandom::fill(&mut salt).map_err(|err| PasswordError::Random(err.to_string()))?;
    let params = Params::new(M_COST_KIB, T_COST, P_COST, None)
        .map_err(|err| PasswordError::Hash(err.to_string()))?;
    let hash = Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_with_salt(password.as_bytes(), &salt)
        .map_err(|err| PasswordError::Hash(err.to_string()))?;
    PasswordHashConfig::from_phc(&hash.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: &str = "test-password-for-unit-tests";

    /// A synthetic PHC string `$<alg>$<rest>` (built, so that no literal
    /// hash prefix appears in the source for the secret scanner to flag).
    fn phc(alg: &str, rest: &str) -> String {
        format!("${alg}${rest}")
    }

    #[test]
    fn r82_hash_password_round_trips_and_verifies() {
        let config = hash_password(PASSWORD).expect("hash");
        assert!(config.verify(PASSWORD));
        assert!(!config.verify("test-password-for-unit-test"));
        assert!(!config.verify(""));
        let text = config.phc();
        assert!(
            text.starts_with(&phc("argon2id", "v=19$m=19456,t=2,p=1$")),
            "{text}"
        );
        // A fresh salt every time: same password, different hash and fingerprint.
        let other = hash_password(PASSWORD).expect("hash");
        assert_ne!(config.fingerprint(), other.fingerprint());
        let reloaded = PasswordHashConfig::from_phc(config.phc()).expect("stored hash");
        assert_eq!(reloaded.fingerprint(), config.fingerprint());
        assert_eq!(format!("{config:?}"), "PasswordHashConfig(***)");
    }

    #[test]
    fn r82_weak_or_malformed_hashes_are_refused() {
        // Synthetic PHC strings (salt/hash values are placeholders).
        let salt_hash = "c29tZXNhbHRzb21lc2FsdA$0123456789abcdefghijklmnopqrstuvwxyzABCDE";
        for (phc, why) in [
            (
                phc("argon2i", &format!("v=19$m=19456,t=2,p=1${salt_hash}")),
                "argon2i",
            ),
            (
                phc("argon2id", &format!("v=16$m=19456,t=2,p=1${salt_hash}")),
                "v16",
            ),
            (
                phc("argon2id", &format!("v=19$m=4096,t=2,p=1${salt_hash}")),
                "m",
            ),
            (
                phc("argon2id", &format!("v=19$m=19456,t=1,p=1${salt_hash}")),
                "t",
            ),
            (
                phc("argon2id", &format!("v=19$m=1048576,t=2,p=1${salt_hash}")),
                "m too big",
            ),
            (phc("argon2id", "v=19$m=19456,t=2,p=1"), "no salt/hash"),
            ("plain text".to_owned(), "not phc"),
        ] {
            assert!(PasswordHashConfig::from_phc(&phc).is_err(), "{why}");
        }
    }

    #[test]
    fn r82_hash_password_refuses_too_short_or_too_long_passwords() {
        assert!(matches!(hash_password(""), Err(PasswordError::Length)));
        assert!(matches!(hash_password("short"), Err(PasswordError::Length)));
        // At least 12 characters (not bytes): 11 two-byte letters are too few.
        assert!(matches!(
            hash_password(&"ř".repeat(11)),
            Err(PasswordError::Length)
        ));
        assert!(check_new(&"ř".repeat(12)).is_ok());
        let long = "x".repeat(MAX_PASSWORD_BYTES + 1);
        assert!(matches!(hash_password(&long), Err(PasswordError::Length)));
    }
}
