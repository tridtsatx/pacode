//! PKCE (RFC 7636) code verifier and challenge generation.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// PKCE code verifier and challenge pair for OAuth 2.0 with S256.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pkce {
    /// High-entropy cryptographic random string (43 characters from 32 bytes).
    pub verifier: String,
    /// Base64URL-nopad SHA-256 hash of the verifier.
    pub challenge: String,
}

impl Pkce {
    /// PKCE code challenge method identifier.
    pub const METHOD: &'static str = "S256";

    /// Generate a fresh 32-byte random PKCE pair with S256 challenge.
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let verifier = URL_SAFE_NO_PAD.encode(bytes);
        let challenge = compute_challenge(&verifier);
        Self {
            verifier,
            challenge,
        }
    }

    /// Return the PKCE method ("S256").
    pub fn method(&self) -> &'static str {
        Self::METHOD
    }
}

/// Compute base64url-nopad(sha256(verifier)).
pub fn compute_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let digest = hasher.finalize();
    URL_SAFE_NO_PAD.encode(digest)
}

/// Generate a cryptographically secure, URL-safe random state token.
///
/// Uses 32 random bytes (256 bits of entropy, well above the 128-bit / 16-byte minimum),
/// encoded with base64url without padding.
pub fn random_state() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
#[path = "pkce_tests.rs"]
mod pkce_tests;
