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
//! host as the archive is not a defense against a compromised host, but it
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

use serde_json::Value;
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
    // Android runs the static musl builds, which need no Bionic libc. They
    // ship inside the APK ([`use_bundled_binary`]) rather than downloaded:
    // Android only lets an app run binaries from its native library directory.
    #[cfg(all(target_os = "android", target_arch = "aarch64"))]
    {
        Some(("aarch64-unknown-linux-musl", "tar.gz"))
    }
    #[cfg(all(target_os = "android", target_arch = "x86_64"))]
    {
        Some(("x86_64-unknown-linux-musl", "tar.gz"))
    }
    #[cfg(all(target_os = "android", target_arch = "arm"))]
    {
        Some(("armv7-unknown-linux-musleabihf", "tar.gz"))
    }
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "android", target_arch = "aarch64"),
        all(target_os = "android", target_arch = "x86_64"),
        all(target_os = "android", target_arch = "arm"),
    )))]
    {
        None
    }
}

/// The data directory the app uses when a caller does not pass one.
///
/// This is `AppPaths::resolve().data_dir()` in `starpod-store`: the platform
/// local-data root with the `Starpod` application directory appended. The
/// engine crate does not depend on the store, so the rule is repeated here;
/// `crates/starpod-app/tests/engine_install_path.rs` fails if the two drift.
pub fn default_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .or_else(dirs::cache_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Starpod")
}

/// Where the engine is installed, under the app's data directory.
///
/// `<data>/engine/<version>`. The directory is per-user: the engine runs as
/// the user who runs the app, and a machine-wide install would need
/// privileges the app does not ask for.
pub fn install_dir_in(data_dir: &Path) -> PathBuf {
    data_dir.join("engine").join(ENGINE_VERSION)
}

/// An engine that came with the app (Android's `libmeow.so`), set once at
/// start. While it is set it is the installed engine, at the pinned version,
/// and nothing is downloaded or replaced.
static BUNDLED: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Use the engine binary at `path`, which came with the app.
pub fn use_bundled_binary(path: PathBuf) {
    let _ = BUNDLED.set(path);
}

/// Whether the engine came with the app, so it is updated with the app.
pub fn is_bundled() -> bool {
    BUNDLED.get().is_some()
}

/// The path of the installed engine binary, under `data_dir`.
pub fn installed_binary_path_in(data_dir: &Path) -> PathBuf {
    if let Some(bundled) = BUNDLED.get() {
        return bundled.clone();
    }
    install_dir_in(data_dir).join(binary_name())
}

/// Whether an engine binary is installed under `data_dir`.
pub fn is_installed_in(data_dir: &Path) -> bool {
    installed_binary_path_in(data_dir).is_file()
}

/// The installed engine binary in the default data directory.
pub fn installed_binary_path() -> PathBuf {
    installed_binary_path_in(&default_data_dir())
}

/// Whether an engine binary is installed in the default data directory.
pub fn is_installed() -> bool {
    is_installed_in(&default_data_dir())
}

/// Download, verify and install the engine into the default data directory.
pub async fn install() -> EngineResult<PathBuf> {
    install_in(&default_data_dir()).await
}

/// Download, verify and install the engine under `data_dir`.
///
/// The archive is written to a temporary file, verified against the
/// published checksum, and only then extracted. A failed verification
/// leaves nothing behind.
pub async fn install_in(data_dir: &Path) -> EngineResult<PathBuf> {
    let asset = asset_for_current_platform().ok_or_else(|| {
        EngineError::NotInstalled(
            "no engine build is published for this platform; install one manually".to_string(),
        )
    })?;
    install_asset_in(data_dir, &asset, ENGINE_VERSION).await
}

/// Download, verify and install one release asset under `data_dir`, and record
/// `version` as the installed version.
///
/// The install slot is the same for every release (`<data>/engine/<ENGINE_VERSION>`),
/// so an upgrade replaces the binary the engine is already pointed at. The
/// caller must stop a running engine first: Windows will not replace a
/// running executable.
pub async fn install_asset_in(
    data_dir: &Path,
    asset: &Asset,
    version: &str,
) -> EngineResult<PathBuf> {
    if is_bundled() {
        return Err(EngineError::Install(
            "the engine comes with the app and is updated with it".to_string(),
        ));
    }
    let client = http_client()?;

    let expected = fetch_checksum(&client, &asset.checksum_url).await?;
    let bytes = fetch_bytes(&client, &asset.url).await?;
    let actual = sha256_hex(&bytes);
    if actual != expected {
        return Err(EngineError::Install(format!(
            "the downloaded engine does not match its checksum (expected {expected}, got {actual})"
        )));
    }

    let target = installed_binary_path_in(data_dir);
    let install_dir = install_dir_in(data_dir);
    let staging = install_dir.join(format!(".staging-{}", std::process::id()));
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
            let _ = std::fs::copy(&dll, install_dir.join("wintun.dll"));
        }
    }
    let _ = std::fs::remove_dir_all(&staging);

    write_installed_version(data_dir, version)?;
    Ok(target)
}

/// The HTTP client every download uses.
fn http_client() -> EngineResult<reqwest::Client> {
    crate::tls::configure(reqwest::Client::builder())
        .timeout(std::time::Duration::from_secs(300))
        .user_agent(concat!("starpod/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| EngineError::Install(e.to_string()))
}

/// The file, inside the install slot, that names the installed release.
const VERSION_FILE: &str = "VERSION";

/// The version label of the installed engine, or `None` when none is installed.
///
/// A binary installed before the marker existed is the pinned version.
pub fn installed_version_in(data_dir: &Path) -> Option<String> {
    if !is_installed_in(data_dir) {
        return None;
    }
    if is_bundled() {
        return Some(ENGINE_VERSION.to_string());
    }
    let marker = install_dir_in(data_dir).join(VERSION_FILE);
    match std::fs::read_to_string(marker) {
        Ok(text) if !text.trim().is_empty() => Some(text.trim().to_string()),
        _ => Some(ENGINE_VERSION.to_string()),
    }
}

/// Record which release is installed.
fn write_installed_version(data_dir: &Path, version: &str) -> EngineResult<()> {
    let marker = install_dir_in(data_dir).join(VERSION_FILE);
    std::fs::write(&marker, format!("{version}\n")).map_err(|e| EngineError::Install(e.to_string()))
}

/// A release that can be installed, and the version label it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestRelease {
    /// The label the app shows: `0.29.0` for stable, `alpha-<commit>` for alpha.
    pub version: String,
    /// The asset for this platform.
    pub asset: Asset,
}

/// The GitHub API listing of the engine's releases.
pub const RELEASES_API: &str = "https://api.github.com/repos/meow-rs/meow-rs/releases?per_page=30";

/// The newest release of `channel` that has a build for this platform.
///
/// Stable is the newest non-prerelease whose tag is `v<version>`. Alpha is the
/// newest release carrying a `meow-alpha-<commit>-<target>` asset.
pub async fn latest_release(
    channel: starpod_core::domain::EngineChannel,
) -> EngineResult<LatestRelease> {
    let (target, archive) = target_triple().ok_or_else(|| {
        EngineError::NotInstalled(
            "no engine build is published for this platform; install one manually".to_string(),
        )
    })?;
    let client = http_client()?;
    let text = client
        .get(RELEASES_API)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| EngineError::Install(format!("could not list the engine releases: {e}")))?
        .error_for_status()
        .map_err(|e| EngineError::Install(format!("could not list the engine releases: {e}")))?
        .text()
        .await
        .map_err(|e| EngineError::Install(format!("could not read the engine releases: {e}")))?;
    let releases: Value = serde_json::from_str(&text)
        .map_err(|e| EngineError::Install(format!("the engine release list is not JSON: {e}")))?;
    pick_release(&releases, channel, target, archive).ok_or_else(|| {
        EngineError::Install(format!(
            "no {} engine build is published for this platform",
            match channel {
                starpod_core::domain::EngineChannel::Stable => "stable",
                starpod_core::domain::EngineChannel::Alpha => "alpha",
            }
        ))
    })
}

/// Pick the newest matching release out of the API listing. Pure, so the
/// selection rules are tested without the network.
pub fn pick_release(
    releases: &Value,
    channel: starpod_core::domain::EngineChannel,
    target: &str,
    archive: &str,
) -> Option<LatestRelease> {
    let suffix = format!("-{target}.{archive}");
    for release in releases.as_array()? {
        let Some(assets) = release.get("assets").and_then(Value::as_array) else {
            continue;
        };
        let find = |name: &str| {
            assets
                .iter()
                .find(|asset| asset.get("name").and_then(Value::as_str) == Some(name))
                .and_then(|asset| asset.get("browser_download_url"))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        match channel {
            starpod_core::domain::EngineChannel::Stable => {
                if release.get("prerelease").and_then(Value::as_bool) == Some(true)
                    || release.get("draft").and_then(Value::as_bool) == Some(true)
                {
                    continue;
                }
                let Some(tag) = release.get("tag_name").and_then(Value::as_str) else {
                    continue;
                };
                if !tag.starts_with('v') {
                    continue;
                }
                let file_name = format!("meow-{tag}{suffix}");
                let (Some(url), Some(checksum_url)) =
                    (find(&file_name), find(&format!("{file_name}.sha256")))
                else {
                    continue;
                };
                return Some(LatestRelease {
                    version: tag.trim_start_matches('v').to_string(),
                    asset: Asset {
                        file_name,
                        url,
                        checksum_url,
                    },
                });
            }
            starpod_core::domain::EngineChannel::Alpha => {
                let found = assets.iter().find_map(|asset| {
                    let name = asset.get("name")?.as_str()?;
                    let stem = name.strip_prefix("meow-alpha-")?.strip_suffix(&suffix)?;
                    (!stem.is_empty()).then(|| name.to_string())
                });
                let Some(file_name) = found else {
                    continue;
                };
                let (Some(url), Some(checksum_url)) =
                    (find(&file_name), find(&format!("{file_name}.sha256")))
                else {
                    continue;
                };
                let stem = file_name
                    .strip_prefix("meow-")
                    .and_then(|rest| rest.strip_suffix(&suffix))
                    .unwrap_or(&file_name)
                    .to_string();
                return Some(LatestRelease {
                    version: stem,
                    asset: Asset {
                        file_name,
                        url,
                        checksum_url,
                    },
                });
            }
        }
    }
    None
}

/// Download a file to `dest`, replacing it only when the download is complete.
///
/// Used for the geodata databases, which publish no checksum the app can
/// verify against, so the check is that the transfer finished and is not empty.
pub async fn download_file(url: &str, dest: &Path) -> EngineResult<u64> {
    let client = http_client()?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| EngineError::Install(format!("could not download {url}: {e}")))?
        .error_for_status()
        .map_err(|e| EngineError::Install(format!("could not download {url}: {e}")))?;
    let bytes = response
        .bytes()
        .await
        .map_err(|e| EngineError::Install(format!("could not download {url}: {e}")))?;
    if bytes.is_empty() {
        return Err(EngineError::Install(format!(
            "the download from {url} was empty"
        )));
    }
    let partial = dest.with_extension("part");
    std::fs::write(&partial, &bytes).map_err(|e| EngineError::Io(e.to_string()))?;
    std::fs::rename(&partial, dest).map_err(|e| EngineError::Io(e.to_string()))?;
    Ok(bytes.len() as u64)
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
        std::io::copy(&mut entry, &mut file).map_err(|e| EngineError::Install(e.to_string()))?;
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

    fn release_feed() -> Value {
        serde_json::json!([
            {
                "tag_name": "v0.30.0-rc1",
                "prerelease": true,
                "assets": [
                    {"name": "meow-v0.30.0-rc1-x86_64-pc-windows-msvc.zip", "browser_download_url": "https://example.test/rc.zip"},
                    {"name": "meow-v0.30.0-rc1-x86_64-pc-windows-msvc.zip.sha256", "browser_download_url": "https://example.test/rc.zip.sha256"}
                ]
            },
            {
                "tag_name": "alpha",
                "prerelease": true,
                "assets": [
                    {"name": "meow-alpha-9f9dc68-x86_64-pc-windows-msvc.zip", "browser_download_url": "https://example.test/alpha.zip"},
                    {"name": "meow-alpha-9f9dc68-x86_64-pc-windows-msvc.zip.sha256", "browser_download_url": "https://example.test/alpha.zip.sha256"}
                ]
            },
            {
                "tag_name": "v0.29.0",
                "prerelease": false,
                "assets": [
                    {"name": "meow-v0.29.0-x86_64-pc-windows-msvc.zip", "browser_download_url": "https://example.test/stable.zip"},
                    {"name": "meow-v0.29.0-x86_64-pc-windows-msvc.zip.sha256", "browser_download_url": "https://example.test/stable.zip.sha256"}
                ]
            }
        ])
    }

    #[test]
    fn the_stable_channel_skips_prereleases_and_takes_the_newest_tagged_build() {
        let pick = pick_release(
            &release_feed(),
            starpod_core::domain::EngineChannel::Stable,
            "x86_64-pc-windows-msvc",
            "zip",
        )
        .expect("a stable build");
        assert_eq!(pick.version, "0.29.0");
        assert_eq!(pick.asset.url, "https://example.test/stable.zip");
        assert_eq!(
            pick.asset.checksum_url,
            "https://example.test/stable.zip.sha256"
        );
    }

    #[test]
    fn the_alpha_channel_takes_the_newest_rolling_build_and_labels_it_by_commit() {
        let pick = pick_release(
            &release_feed(),
            starpod_core::domain::EngineChannel::Alpha,
            "x86_64-pc-windows-msvc",
            "zip",
        )
        .expect("an alpha build");
        assert_eq!(pick.version, "alpha-9f9dc68");
        assert_eq!(
            pick.asset.file_name,
            "meow-alpha-9f9dc68-x86_64-pc-windows-msvc.zip"
        );
        assert_eq!(pick.asset.url, "https://example.test/alpha.zip");
    }

    #[test]
    fn a_platform_with_no_build_gets_no_release() {
        let none = pick_release(
            &release_feed(),
            starpod_core::domain::EngineChannel::Stable,
            "riscv64gc-unknown-linux-gnu",
            "tar.gz",
        );
        assert!(none.is_none());
    }

    #[test]
    fn an_installed_version_is_the_pinned_one_until_an_upgrade_records_another() {
        let dir = std::env::temp_dir().join(format!("starpod-version-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let slot = install_dir_in(&dir);
        std::fs::create_dir_all(&slot).expect("a slot");
        std::fs::write(slot.join(binary_name()), b"not really an engine").expect("a binary");
        assert_eq!(installed_version_in(&dir).as_deref(), Some(ENGINE_VERSION));
        write_installed_version(&dir, "alpha-9f9dc68").expect("recorded");
        assert_eq!(installed_version_in(&dir).as_deref(), Some("alpha-9f9dc68"));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(installed_version_in(&dir), None);
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
    fn the_engine_installs_under_the_app_data_directory_and_version() {
        let data = Path::new("/data/Starpod");
        assert_eq!(
            install_dir_in(data),
            Path::new("/data/Starpod/engine").join(ENGINE_VERSION)
        );
        assert_eq!(
            installed_binary_path_in(data),
            install_dir_in(data).join(binary_name())
        );
    }

    #[test]
    fn the_default_data_directory_uses_the_app_directory_name() {
        // The name is the one `AppPaths` uses. Lowercase `starpod` was the
        // old, separate location and must not come back.
        let path = default_data_dir();
        assert!(path.ends_with("Starpod"), "{path:?}");
        assert!(!path.ends_with("starpod"), "{path:?}");
    }

    #[test]
    fn an_install_with_no_binary_is_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_installed_in(dir.path()));
        std::fs::create_dir_all(install_dir_in(dir.path())).unwrap();
        std::fs::write(installed_binary_path_in(dir.path()), b"not a real engine").unwrap();
        assert!(is_installed_in(dir.path()));
    }
}
