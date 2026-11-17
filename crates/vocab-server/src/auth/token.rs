//! The connector's import token (design §9.2).
//!
//! Only its SHA-256 digest is kept; a presented token is hashed the same way
//! and the digests are compared in constant time, so neither the comparison
//! time nor the token length leaks anything.

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Shortest accepted token (e.g. `openssl rand -hex 32` gives 64).
pub const MIN_TOKEN_CHARS: usize = 32;
/// Longest accepted token; longer values are configuration mistakes.
const MAX_TOKEN_CHARS: usize = 512;

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error(
        "must be {MIN_TOKEN_CHARS} to {MAX_TOKEN_CHARS} printable ASCII characters without spaces"
    )]
    Invalid,
}

/// The configured token, as a digest. `Debug` never shows it.
#[derive(Clone)]
pub struct ImportToken {
    digest: [u8; 32],
}

impl std::fmt::Debug for ImportToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ImportToken(***)")
    }
}

impl ImportToken {
    pub fn new(token: &str) -> Result<Self, TokenError> {
        if !(MIN_TOKEN_CHARS..=MAX_TOKEN_CHARS).contains(&token.len())
            || !token.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(TokenError::Invalid);
        }
        Ok(Self {
            digest: Sha256::digest(token.as_bytes()).into(),
        })
    }

    /// Whether `presented` is the configured token.
    pub fn matches(&self, presented: &str) -> bool {
        let digest: [u8; 32] = Sha256::digest(presented.as_bytes()).into();
        digest.ct_eq(&self.digest).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "test-import-token-0123456789abcdef";

    #[test]
    fn r82_import_token_must_be_long_printable_ascii() {
        assert!(ImportToken::new(TOKEN).is_ok());
        assert!(ImportToken::new(&TOKEN[..31]).is_err());
        assert!(ImportToken::new(&"x".repeat(32)).is_ok());
        assert!(ImportToken::new("test import token with spaces 0123456").is_err());
        assert!(ImportToken::new("test-import-token-ěščř-0123456789abc").is_err());
        assert!(ImportToken::new(&"x".repeat(513)).is_err());
    }

    #[test]
    fn r82_import_token_matches_only_itself() {
        let token = ImportToken::new(TOKEN).expect("valid");
        assert!(token.matches(TOKEN));
        assert!(!token.matches(&TOKEN[..TOKEN.len() - 1]));
        assert!(!token.matches(&format!("{TOKEN}x")));
        assert!(!token.matches(""));
        assert_eq!(format!("{token:?}"), "ImportToken(***)");
    }
}
