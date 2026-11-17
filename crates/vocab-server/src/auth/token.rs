//! The connector token (design §9.2) and the one-time setup code (§9.1).
//!
//! Both are random secrets the app makes itself and keeps only as SHA-256
//! digests. A presented value is hashed the same way and the digests are
//! compared in constant time, so neither the comparison time nor the length
//! of a guess leaks anything.

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Random bytes of a connector token (shown as 64 hex digits).
const TOKEN_BYTES: usize = 32;
/// Characters of a setup code: Crockford base32 (no I, L, O, U), so it can
/// be read from a log and typed on a phone without confusion.
const SETUP_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// Length of a setup code (50 bits; the login throttle limits guessing).
const SETUP_CODE_CHARS: usize = 10;

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn random<const N: usize>() -> Result<[u8; N], String> {
    let mut raw = [0u8; N];
    getrandom::fill(&mut raw).map_err(|err| err.to_string())?;
    Ok(raw)
}

/// The connector token, as a digest. `Debug` never shows it.
#[derive(Clone)]
pub struct ConnectorToken {
    digest: [u8; 32],
}

impl std::fmt::Debug for ConnectorToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConnectorToken(***)")
    }
}

impl ConnectorToken {
    /// A new random token: the value to show once, and its digest to keep.
    pub fn generate() -> Result<(String, Self), String> {
        let token = hex::encode(random::<TOKEN_BYTES>()?);
        let digest = sha256(token.as_bytes());
        Ok((token, Self { digest }))
    }

    /// From the stored digest (64 hex digits).
    pub fn from_digest_hex(text: &str) -> Option<Self> {
        let mut digest = [0u8; 32];
        hex::decode_to_slice(text, &mut digest).ok()?;
        Some(Self { digest })
    }

    /// The digest to store.
    pub fn digest_hex(&self) -> String {
        hex::encode(self.digest)
    }

    /// Whether `presented` is this token.
    pub fn matches(&self, presented: &str) -> bool {
        sha256(presented.as_bytes()).ct_eq(&self.digest).into()
    }
}

/// A setup code as the owner reads it in the log: `XXXX-XXXX-XX`. `Display`
/// shows it (for that one log line); `Debug` never does.
#[derive(Clone)]
pub struct SetupCode(String);

impl std::fmt::Debug for SetupCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SetupCode(***)")
    }
}

impl std::fmt::Display for SetupCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (a, rest) = self.0.split_at(4);
        let (b, c) = rest.split_at(4);
        write!(f, "{a}-{b}-{c}")
    }
}

impl SetupCode {
    /// A new random code and the digest to keep.
    pub fn generate() -> Result<(Self, [u8; 32]), String> {
        // 32 symbols divide 256: masking a byte to 5 bits has no bias.
        let code: String = random::<SETUP_CODE_CHARS>()?
            .iter()
            .map(|b| char::from(SETUP_ALPHABET[usize::from(b & 31)]))
            .collect();
        let digest = sha256(code.as_bytes());
        Ok((Self(code), digest))
    }
}

/// Does `typed` match the code of `digest`? Case, spaces and dashes do not
/// matter, and the letters Crockford base32 leaves out are read as the
/// digits they look like (O → 0, I and L → 1).
pub fn setup_code_matches(typed: &str, digest: &[u8; 32]) -> bool {
    let canonical: String = typed
        .chars()
        .filter(|c| !matches!(c, '-' | ' '))
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            other => other,
        })
        .take(SETUP_CODE_CHARS + 1)
        .collect();
    sha256(canonical.as_bytes()).ct_eq(digest).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r82_connector_token_is_random_hex_and_matches_only_itself() {
        let (token, stored) = ConnectorToken::generate().expect("random");
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(stored.matches(&token));
        assert!(!stored.matches(&token[..63]));
        assert!(!stored.matches(&format!("{token}0")));
        assert!(!stored.matches(&token.to_uppercase()));
        assert!(!stored.matches(""));
        assert_eq!(format!("{stored:?}"), "ConnectorToken(***)");
        let reloaded = ConnectorToken::from_digest_hex(&stored.digest_hex()).expect("digest");
        assert!(reloaded.matches(&token));
        assert_ne!(stored.digest_hex(), token, "only the digest is stored");
        assert!(ConnectorToken::from_digest_hex("zz").is_none());
        let (other, _) = ConnectorToken::generate().expect("random");
        assert_ne!(token, other);
    }

    #[test]
    fn r82_setup_code_is_grouped_crockford_base32_and_forgiving_when_typed() {
        let (code, digest) = SetupCode::generate().expect("random");
        let shown = code.to_string();
        assert_eq!(shown.len(), 12, "{shown}");
        assert_eq!(&shown[4..5], "-");
        assert_eq!(&shown[9..10], "-");
        assert!(
            shown
                .bytes()
                .filter(|b| *b != b'-')
                .all(|b| SETUP_ALPHABET.contains(&b)),
            "{shown}"
        );
        assert_eq!(format!("{code:?}"), "SetupCode(***)");
        assert!(setup_code_matches(&shown, &digest));
        assert!(setup_code_matches(&shown.to_lowercase(), &digest));
        assert!(setup_code_matches(&shown.replace('-', " "), &digest));
        assert!(!setup_code_matches(&shown[..11], &digest));
        assert!(!setup_code_matches(&format!("{shown}0"), &digest));
        assert!(!setup_code_matches("", &digest));

        let (fixed, digest) = (SetupCode("0123ABCD1Z".to_owned()), sha256(b"0123ABCD1Z"));
        assert_eq!(fixed.to_string(), "0123-ABCD-1Z");
        assert!(setup_code_matches("o123-abcd-lz", &digest));
        assert!(setup_code_matches("O123ABCDIZ", &digest));
    }
}
