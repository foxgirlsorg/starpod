//! TLS roots for the HTTP clients.
//!
//! On the desktop, reqwest checks certificates with the operating system's
//! verifier (rustls-platform-verifier). On Android that verifier must be
//! initialised from Java, and Starpod runs as a NativeActivity with no Java
//! code, so building a client there would abort. Android keeps its trusted
//! roots as PEM files, so on Android the clients trust exactly those files
//! instead: the updatable Conscrypt store, or the system store before it.
//!
//! `starpod-engine` keeps its own copy of this file, since it does not depend
//! on this crate.

/// Apply this platform's certificate roots to a client builder. On every
/// platform but Android this changes nothing.
pub fn configure(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    #[cfg(target_os = "android")]
    {
        builder.tls_certs_only(android_roots().iter().cloned())
    }
    #[cfg(not(target_os = "android"))]
    {
        builder
    }
}

/// The system's trusted roots, read once.
#[cfg(target_os = "android")]
fn android_roots() -> &'static [reqwest::Certificate] {
    use std::sync::OnceLock;
    static ROOTS: OnceLock<Vec<reqwest::Certificate>> = OnceLock::new();
    ROOTS.get_or_init(|| {
        // Android 14 and later update the roots through the Conscrypt APEX;
        // older releases only have the system directory.
        let dirs = [
            "/apex/com.android.conscrypt/cacerts",
            "/system/etc/security/cacerts",
        ];
        let mut roots = Vec::new();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                if let Ok(pem) = std::fs::read(entry.path())
                    && let Ok(cert) = reqwest::Certificate::from_pem(&pem)
                {
                    roots.push(cert);
                }
            }
            if !roots.is_empty() {
                break;
            }
        }
        roots
    })
}
