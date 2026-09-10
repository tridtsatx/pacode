use super::*;

#[test]
fn test_rfc7636_known_vector() {
    // RFC 7636 Appendix B test vector:
    // code_verifier: dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk
    // code_challenge: E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let expected_challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    let challenge = compute_challenge(verifier);
    assert_eq!(challenge, expected_challenge);
}

#[test]
fn test_pkce_generate_charset() {
    for _ in 0..100 {
        let pkce = Pkce::generate();
        assert_eq!(pkce.method(), "S256");

        for ch in ['=', '+', '/'] {
            assert!(
                !pkce.verifier.contains(ch),
                "verifier contains forbidden char '{ch}': {}",
                pkce.verifier
            );
            assert!(
                !pkce.challenge.contains(ch),
                "challenge contains forbidden char '{ch}': {}",
                pkce.challenge
            );
        }

        // Must match compute_challenge
        assert_eq!(compute_challenge(&pkce.verifier), pkce.challenge);
    }
}

#[test]
fn test_random_state_charset_and_entropy() {
    for _ in 0..100 {
        let state = random_state();
        // 32 bytes encoded in base64 without padding is 43 characters
        assert_eq!(state.len(), 43);

        for ch in ['=', '+', '/'] {
            assert!(
                !state.contains(ch),
                "state contains forbidden char '{ch}': {state}"
            );
        }

        // Characters must be base64url characters: [A-Za-z0-9-_]
        for c in state.chars() {
            assert!(
                c.is_ascii_alphanumeric() || c == '-' || c == '_',
                "invalid char in state: '{c}'"
            );
        }
    }
}
