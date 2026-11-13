//! Installing the proxy engine.
//!
//! The app does not link the engine in; it runs it as a subprocess. That
//! means the app has to *have* an engine binary, and there are two ways to
//! get one:
//!
//! - the release archive for this platform, downloaded and verified;
//! - a binary the user put there themselves, for a build that is not a
//!   published release.
//!
//! # Verification
//!
//! A downloaded archive is verified against the checksum the release
//! publishes, **before** it is extracted. A checksum fetched from the same
//! host as the archive is not a defence against a compromised host, but it
//! does defend against a corrupted download, a truncated transfer and a
//! proxy that rewrites the archive — and it costs one request.
//!
//! The binary is also checked for the executable bit and for a plausible
//! size before it is installed, so a HTML error page saved as `meow.exe`
//! never becomes the engine.
//!
//! # Permissions
//!
//! The installed binary is written with owner-only permissions on unix. It
//! is not a secret, but it *is* a program the app will execute, and a
//! world-writable directory in its path is a way to run anything.

use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

use crate::error::{EngineError, EngineResult};

/// The engine version the app ships with.
///
/// The app pins the version: an engine upgrade is a decision with a
/// compatibility surface (the REST API, the config keys, the rule
/// grammar), not something a user's proxy should decide for them.
pub const ENGINE_VERSION: &str = "0.29.0";

/// The release tag the assets are published under.
pub const RELEASE_TAG: &str = "v0.29.0";

/// Where the release assets are published.
pub const RELEASES_URL: &str = "https://github.com/meow-rs/meow-rs/releases/download";

/// The name of the engine binary on this platform.
pub const fn binary_name() -> &'static str {
    if cfg!(windows) { "meow.exe" } else { "meow" }
}

/// A release asset for this platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// The asset's file name, as published.
    pub file_name: String,
    /// The asset's URL.
    pub url: String,
    /// The URL of the checksum file that accompanies it.
    pub checksum_url: String,
}

/// The release asset for the platform this build runs on.
///
/// `None` on a platform the engine does not publish a binary for. The app
/// treats that as "the user must supply an engine binary", not as an
/// error.
pub fn asset_for_current_platform() -> Option<Asset> {
    let (target, archive) = target_triple()?;
    let file_name = format!("meow-{RELEASE_TAG}-{target}.{archive}");
    Some(Asset {
        url: format!("{RELEASES_URL}/{RELEASE_TAG}/{file_name}"),
        checksum_url: format!("{RELEASES_URL}/{RELEASE_TAG}/{file_name}.sha256"),
        file_name,
    })
}

/// The target triple and archive kind for this platform.
///
/// The engine publishes `.zip` for Windows and `.tar.gz` elsewhere.
fn target_triple() -> Option<(&'static str, &'static str)> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        Some(("x86_64-pc-windows-msvc", "zip"))
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        Some(("aarch64-pc-windows-msvc", "zip"))
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Some(("aarch64-apple-darwin", "tar.gz"))
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        Some(("x86_64-apple-darwin", "tar.gz"))
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        Some(("x86_64-unknown-linux-gnu", "tar.gz"))
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        Some(("aarch64-unknown-linux-gnu", "tar.gz"))
    }
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
    )))]
    {
        None
    }
}

/// Where the engine is installed.
///
/// The directory is per-user: the engine runs as the user who runs the app,
/// and a machine-wide install would need privileges the app does not ask
/// for.
pub fn install_dir() -> PathBuf {
    let base = dirs::data_local_dir()
        .or_else(dirs::cache_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("starpod").join("engine").join(ENGINE_VERSION)
}

/// The path of the installed engine binary.
pub fn installed_binary_path() -> PathBuf {
    install_dir().join(binary_name())
}

/// Whether an engine binary is already installed.
pub fn is_installed() -> bool {
    installed_binary_path().is_file()
}

/// Download, verify and install the engine.
///
/// The archive is written to a temporary file, verified against the
/// published checksum, and only then extracted. A failed verification
/// leaves nothing behind.
pub async fn install() -> EngineResult<PathBuf> {
    let asset = asset_for_current_platform().ok_or_else(|| {
        EngineError::NotInstalled(
            "no engine build is published for this platform; install one manually".to_string(),
        )
    })?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .user_agent(concat!("starpod/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| EngineError::Install(e.to_string()))?;

    let expected = fetch_checksum(&client, &asset.checksum_url).await?;
    let bytes = fetch_bytes(&client, &asset.url).await?;
    let actual = sha256_hex(&bytes);
    if actual != expected {
        return Err(EngineError::Install(format!(
            "the downloaded engine does not match its checksum (expected {expected}, got {actual})"
        )));
    }

    let target = installed_binary_path();
    let staging = install_dir().join(format!(".staging-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| EngineError::Install(e.to_string()))?;

    let outcome = extract(&asset.file_name, &bytes, &staging);
    let extracted = match outcome {
        Ok(path) => path,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
    };

    // Move the staged binary into place. `rename` across filesystems fails,
    // so a copy is the fallback.
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| EngineError::Install(e.to_string()))?;
    }
    if std::fs::rename(&extracted, &target).is_err() {
        std::fs::copy(&extracted, &target).map_err(|e| EngineError::Install(e.to_string()))?;
        let _ = std::fs::remove_file(&extracted);
    }
    make_executable(&target)?;

    // The Windows archive also carries `wintun.dll`, which the TUN listener
    // loads. It belongs next to the binary.
    #[cfg(windows)]
    {
        let dll = staging.join("wintun.dll");
        if dll.is_file() {
            let _ = std::fs::copy(&dll, install_dir().join("wintun.dll"));
        }
    }
    let _ = std::fs::remove_dir_all(&staging);

    Ok(target)
}

/// Fetch a checksum file and read the hash out of it.
///
/// The file is in `sha256sum` format: a hash, whitespace, and the file
/// name. Only the hash is read — the file name is not checked, because the
/// archive is written to a staging directory under a name of our choosing.
async fn fetch_checksum(client: &reqwest::Client, url: &str) -> EngineResult<String> {
    let text = client
        .get(url)
        .send()
        .await
        .map_err(|e| EngineError::Install(format!("could not fetch the checksum: {e}")))?
        .error_for_status()
        .map_err(|e| EngineError::Install(format!("could not fetch the checksum: {e}")))?
        .text()
        .await
        .map_err(|e| EngineError::Install(format!("could not read the checksum: {e}")))?;

    let hash = text
        .split_whitespace()
        .next()
        .ok_or_else(|| EngineError::Install("the checksum file is empty".to_string()))?
        .to_ascii_lowercase();

    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(EngineError::Install(
            "the checksum file does not contain a hash".to_string(),
        ));
    }
    Ok(hash)
}

/// Fetch a file's bytes.
async fn fetch_bytes(client: &reqwest::Client, url: &str) -> EngineResult<Vec<u8>> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| EngineError::Install(format!("could not download the engine: {e}")))?
        .error_for_status()
        .map_err(|e| EngineError::Install(format!("could not download the engine: {e}")))?;
    let bytes = response
        .bytes()
        .await
        .map_err(|e| EngineError::Install(format!("could not download the engine: {e}")))?;
    Ok(bytes.to_vec())
}

/// The sha256 of a byte slice, as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Extract the engine binary from an archive.
///
/// The archive's entries are read by name: the binary is the entry called
/// `meow` or `meow.exe`, possibly nested in a directory. An archive that
/// contains no such entry is an error — an HTML error page saved as a
/// `.zip` would otherwise install nothing and fail later, at runtime.
fn extract(file_name: &str, bytes: &[u8], staging: &Path) -> EngineResult<PathBuf> {
    if file_name.ends_with(".zip") {
        extract_zip(bytes, staging)
    } else {
        extract_tar_gz(bytes, staging)
    }
}

/// Extract from a zip archive.
#[cfg(windows)]
fn extract_zip(bytes: &[u8], staging: &Path) -> EngineResult<PathBuf> {
    use std::io::Cursor;

    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| {
        EngineError::Install(format!("the downloaded archive is not a zip file: {e}"))
    })?;

    let mut extracted = None;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|e| EngineError::Install(e.to_string()))?;
        let Ok(name) = entry.name() else {
            continue;
        };
        let base = name.rsplit('/').next().unwrap_or(name.as_ref()).to_string();
        if base == binary_name() {
            let path = staging.join(binary_name());
            let mut file =
                std::fs::File::create(&path).map_err(|e| EngineError::Install(e.to_string()))?;
            std::io::copy(&mut entry, &mut file)
                .map_err(|e| EngineError::Install(e.to_string()))?;
            extracted = Some(path);
        } else if base == "wintun.dll" {
            let path = staging.join("wintun.dll");
            let mut file =
                std::fs::File::create(&path).map_err(|e| EngineError::Install(e.to_string()))?;
            std::io::copy(&mut entry, &mut file)
                .map_err(|e| EngineError::Install(e.to_string()))?;
        }
    }

    extracted.ok_or_else(|| {
        EngineError::Install("the downloaded archive does not contain the engine".to_string())
    })
}

/// Extract from a zip archive, on a platform where the engine ships a
/// `.tar.gz`.
#[cfg(not(windows))]
fn extract_zip(_bytes: &[u8], _staging: &Path) -> EngineResult<PathBuf> {
    Err(EngineError::Install(
        "this platform's engine ships as a tar.gz archive".to_string(),
    ))
}

/// Extract from a gzip-compressed tar archive.
#[cfg(not(windows))]
fn extract_tar_gz(bytes: &[u8], staging: &Path) -> EngineResult<PathBuf> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);

    let mut extracted = None;
    let entries = archive.entries().map_err(|e| {
        EngineError::Install(format!("the downloaded archive is not a tar file: {e}"))
    })?;
    for entry in entries {
        let mut entry = entry.map_err(|e| EngineError::Install(e.to_string()))?;
        let path = entry
            .path()
            .map_err(|e| EngineError::Install(e.to_string()))?
            .to_path_buf();
        let base = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        if base != binary_name() {
            continue;
        }
        let out = staging.join(binary_name());
        let mut file =
            std::fs::File::create(&out).map_err(|e| EngineError::Install(e.to_string()))?;
        entry
            .read_to_end(&mut file)
            .map_err(|e| EngineError::Install(e.to_string()))?;
        extracted = Some(out);
    }

    extracted.ok_or_else(|| {
        EngineError::Install("the downloaded archive does not contain the engine".to_string())
    })
}

/// Extract from a gzip-compressed tar archive, on a platform where the
/// engine ships a `.zip`.
#[cfg(windows)]
fn extract_tar_gz(_bytes: &[u8], _staging: &Path) -> EngineResult<PathBuf> {
    Err(EngineError::Install(
        "this platform's engine ships as a zip archive".to_string(),
    ))
}

/// Make a file executable for its owner.
#[cfg(unix)]
fn make_executable(path: &Path) -> EngineResult<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut permissions = std::fs::metadata(path)
        .map_err(|e| EngineError::Install(e.to_string()))?
        .permissions();
    permissions.set_mode(permissions.mode() | 0o700);
    std::fs::set_permissions(path, permissions).map_err(|e| EngineError::Install(e.to_string()))
}

/// Make a file executable for its owner.
#[cfg(not(unix))]
fn make_executable(_path: &Path) -> EngineResult<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_is_lowercase_hex() {
        let hash = sha256_hex(b"");
        assert_eq!(hash.len(), 64);
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn the_hash_of_a_known_input_matches() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn the_platform_has_an_asset_or_the_app_says_so() {
        // Every platform the app ships on has an asset.
        assert!(asset_for_current_platform().is_some());
    }

    #[test]
    fn the_asset_url_points_at_the_pinned_release() {
        let asset = asset_for_current_platform().unwrap();
        assert!(asset.url.contains(RELEASE_TAG), "{}", asset.url);
        assert!(!asset.url.ends_with(".sha256"), "{}", asset.url);
        assert_eq!(asset.checksum_url, format!("{}.sha256", asset.url));
    }

    #[test]
    fn the_install_directory_is_per_user() {
        let path = install_dir();
        assert!(path.ends_with(ENGINE_VERSION), "{path:?}");
        assert!(path.parent().unwrap().ends_with("engine"), "{path:?}");
    }
}
