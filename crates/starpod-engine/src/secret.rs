//! Generating the controller's secret.
//!
//! The engine's REST API is protected by a bearer secret. The app generates
//! one per run and never shows it.
//!
//! The secret is 32 bytes of randomness, hex-encoded. It is generated from
//! the operating system's random source, not from a general-purpose
//! generator's seed: a predictable secret on a loopback-only controller is
//! still a secret that a local attacker could guess, and the cost of
//! getting this right is one line.

/// Generate a fresh controller secret.
///
/// The result is 64 lowercase hex characters — 32 bytes of entropy.
pub fn random_secret() -> String {
    use rand::RngCore as _;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_secret_is_64_hex_characters() {
        let secret = random_secret();
        assert_eq!(secret.len(), 64, "{secret}");
        assert!(secret.chars().all(|c| c.is_ascii_hexdigit()), "{secret}");
        assert!(secret.chars().all(|c| !c.is_ascii_uppercase()), "{secret}");
    }

    #[test]
    fn two_secrets_differ() {
        assert_ne!(random_secret(), random_secret());
    }
}
