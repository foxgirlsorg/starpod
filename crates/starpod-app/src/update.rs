//! Updates from GitHub Releases: the check, and the over-the-air install.
//!
//! The check asks GitHub for the latest release and compares its tag with the
//! running version (ARCHITECTURE §6.4). It only reads.
//!
//! The install runs only when the user asks for it. It downloads this
//! platform's archive from the release, the one `build.yml` publishes, checks
//! it against the release's own `.sha256` file, takes the binary out, and puts
//! it in place of the running one. The app then quits and starts the new
//! binary. The release is not signed, so the checksum catches a damaged
//! download, not a compromised release.
//!
//! On Android the update is the release's APK, checked the same way and
//! handed to Android's installer ([`AndroidHooks`]), which asks the user and
//! checks that it is signed with the same key as the installed app.

use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::header::{ACCEPT, USER_AGENT};
use semver::Version;
use serde::Deserialize;

/// The GitHub repository the releases come from. Hard-coded: the update check
/// must not follow a link a settings file could change.
pub const RELEASE_REPO: &str = "foxgirlsorg/starpod";

/// The endpoint for the newest published (non-draft, non-prerelease) release.
pub const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/foxgirlsorg/starpod/releases/latest";

/// The version this binary was built as.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How long the request may take before the check gives up.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// How long the archive download may take.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// A release the check found: its version without the leading `v`, the page
/// to show the user, and the files it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// The version, for example `0.2.0`.
    pub version: String,
    /// The release's web page (`html_url`).
    pub url: String,
    /// The files attached to the release.
    pub assets: Vec<ReleaseAsset>,
}

/// One file attached to a release.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReleaseAsset {
    /// The file name, for example `starpod-windows-x86_64.zip`.
    pub name: String,
    /// Where to download it.
    #[serde(rename = "browser_download_url")]
    pub url: String,
}

/// The fields of the GitHub response the check reads.
#[derive(Deserialize)]
struct ReleaseBody {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

/// Parse a version from a release tag. A leading `v` is allowed. The tag must
/// be a full semver version (`1.2.3`, optionally with a pre-release part).
pub fn parse_version(tag: &str) -> Result<Version, String> {
    let text = tag.trim().trim_start_matches('v');
    Version::parse(text).map_err(|error| format!("“{tag}” is not a version: {error}"))
}

/// Whether `latest` is a newer version than `current`. Both may carry a
/// leading `v`. A pre-release is older than its release (`0.2.0-rc1` is older
/// than `0.2.0`), as semver defines it.
pub fn is_newer(latest: &str, current: &str) -> Result<bool, String> {
    Ok(parse_version(latest)? > parse_version(current)?)
}

/// Read a release from the GitHub response body.
pub fn parse_release(body: &str) -> Result<Release, String> {
    let parsed: ReleaseBody = serde_json::from_str(body)
        .map_err(|error| format!("the release could not be read: {error}"))?;
    let version = parse_version(&parsed.tag_name)?.to_string();
    Ok(Release {
        version,
        url: parsed.html_url,
        assets: parsed.assets,
    })
}

/// Fetch the latest release from GitHub. The request carries a User-Agent,
/// which the GitHub API requires.
pub async fn fetch_latest_release() -> Result<Release, String> {
    let client = starpod_subscription::tls::configure(reqwest::Client::builder())
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|error| format!("the request could not be made: {error}"))?;
    let response = client
        .get(LATEST_RELEASE_URL)
        .header(USER_AGENT, format!("starpod/{CURRENT_VERSION}"))
        .header(ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|error| format!("GitHub could not be reached: {error}"))?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        // GitHub answers 404 both when nothing is published and when the
        // repository is private (to a request without a token).
        return Err(format!(
            "GitHub has no published Starpod release (the repository {RELEASE_REPO} has none, or is not public)"
        ));
    }
    if !status.is_success() {
        return Err(format!("GitHub answered {status}"));
    }
    let body = response
        .text()
        .await
        .map_err(|error| format!("the answer could not be read: {error}"))?;
    parse_release(&body)
}

// ----- the over-the-air install -----------------------------------------------

/// The archives `build.yml` publishes for this platform, as `(portable,
/// legacy)`: the portable archive the update installs from, and the name
/// releases up to 0.0.3 used, before `-portable` was added. `None` where no
/// build is published.
fn platform_archives() -> Option<(&'static str, &'static str)> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some((
            "starpod-windows-x86_64-portable.zip",
            "starpod-windows-x86_64.zip",
        ))
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some((
            "starpod-linux-x86_64-portable.tar.gz",
            "starpod-linux-x86_64.tar.gz",
        ))
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some((
            "starpod-macos-aarch64-portable.tar.gz",
            "starpod-macos-aarch64.tar.gz",
        ))
    } else {
        None
    }
}

/// The APK `build.yml` publishes for Android.
pub const ANDROID_APK: &str = "starpod-android.apk";

/// Hands an APK to Android's installer; see [`AndroidHooks::install`].
pub type InstallApk = dyn Fn(&Path) -> Result<(), String> + Send + Sync;

/// What the Android app gives the update code, which cannot reach Java on
/// its own.
pub struct AndroidHooks {
    /// The package of the app that installed Starpod, if Android knows it.
    pub installer: Option<String>,
    /// Hand a downloaded APK to Android's installer. Returns once Android
    /// has it; Android then asks the user.
    pub install: Box<InstallApk>,
}

static ANDROID: std::sync::OnceLock<AndroidHooks> = std::sync::OnceLock::new();

/// Set the Android hooks, once, at start.
pub fn set_android_hooks(hooks: AndroidHooks) {
    let _ = ANDROID.set(hooks);
}

/// Whether Starpod was installed straight from an APK (opened from a
/// browser or a file manager, which Android's own installer installs, or
/// `adb install`) rather than by a store or an updater such as F-Droid,
/// Obtainium or Google Play, which then delivers the updates itself.
pub fn android_installed_directly(installer: Option<&str>) -> bool {
    match installer {
        None | Some("") | Some("com.android.shell") => true,
        // com.android.packageinstaller, com.google.android.packageinstaller
        // and the phone makers' own, such as com.miui.packageinstaller.
        Some(package) => package.ends_with(".packageinstaller"),
    }
}

/// The store or package manager that installed Starpod, if one did. It then
/// delivers the updates, so Starpod never checks for, offers or installs
/// one itself: replacing its files would fight the package manager, and an
/// APK from a store is signed with the store's key, which Android would
/// refuse to replace with ours.
pub fn package_manager() -> Option<&'static str> {
    if cfg!(target_os = "android") {
        return match ANDROID.get() {
            Some(hooks) if android_installed_directly(hooks.installer.as_deref()) => None,
            _ => Some("an app store"),
        };
    }
    if std::env::var_os("FLATPAK_ID").is_some() {
        return Some("Flatpak");
    }
    if std::env::var_os("SNAP").is_some() {
        return Some("Snap");
    }
    let exe = std::env::current_exe().ok()?;
    package_manager_for(&exe.to_string_lossy(), cfg!(target_os = "linux"))
}

/// [`package_manager`] for the executable at `path`. On Linux (`linux`),
/// anything under `/usr` but outside `/usr/local` belongs to the system's
/// package manager (the `.deb`, a distribution package, the AUR).
pub fn package_manager_for(path: &str, linux: bool) -> Option<&'static str> {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    const KNOWN: [(&str, &str); 7] = [
        ("/scoop/apps/", "Scoop"),
        ("/winget/packages/", "WinGet"),
        ("/windowsapps/", "Microsoft Store"),
        ("/cellar/", "Homebrew"),
        ("/caskroom/", "Homebrew"),
        ("/nix/store/", "Nix"),
        ("/snap/", "Snap"),
    ];
    if let Some((_, name)) = KNOWN.iter().find(|(part, _)| path.contains(part)) {
        return Some(name);
    }
    if linux && path.starts_with("/usr/") && !path.starts_with("/usr/local/") {
        return Some("the system package manager");
    }
    None
}

/// The portable archive `build.yml` publishes for this platform, or `None`
/// where no build is published. The installers (`-setup`) are never used
/// for an update.
pub fn archive_name() -> Option<&'static str> {
    platform_archives().map(|(portable, _)| portable)
}

/// The app binary's file name inside the archive.
pub fn binary_name() -> &'static str {
    if cfg!(windows) {
        "starpod.exe"
    } else {
        "starpod"
    }
}

/// The archive for this platform and its checksum file, from the release.
pub fn pick_assets<'a>(
    release: &'a Release,
    archive: &str,
) -> Result<(&'a ReleaseAsset, &'a ReleaseAsset), String> {
    let find = |name: &str| release.assets.iter().find(|asset| asset.name == name);
    let checksum_name = format!("{archive}.sha256");
    match (find(archive), find(&checksum_name)) {
        (Some(archive), Some(checksum)) => Ok((archive, checksum)),
        (None, _) => Err(format!(
            "release {} has no build for this computer ({archive})",
            release.version
        )),
        (Some(_), None) => Err(format!(
            "release {} has no checksum for {archive}, so it cannot be checked",
            release.version
        )),
    }
}

/// The platform's archive and its checksum, from the release. The portable
/// name is preferred; a release that predates `-portable` is taken by its
/// legacy name. If neither is there, the error names the portable archive.
pub fn pick_platform_assets(release: &Release) -> Result<(&ReleaseAsset, &ReleaseAsset), String> {
    if cfg!(target_os = "android") {
        return pick_assets(release, ANDROID_APK);
    }
    let (portable, legacy) = platform_archives()
        .ok_or_else(|| "no Starpod build is published for this computer".to_string())?;
    match pick_assets(release, portable) {
        Ok(pair) => Ok(pair),
        Err(error) => pick_assets(release, legacy).map_err(|_| error),
    }
}

/// The hash in a `sha256sum` line (`<hash>  <file>`).
pub fn parse_checksum(text: &str) -> Result<String, String> {
    let hash = text
        .split_whitespace()
        .next()
        .ok_or("the checksum file is empty")?
        .to_ascii_lowercase();
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("the checksum file does not hold a SHA-256 hash".to_string());
    }
    Ok(hash)
}

/// The SHA-256 of `bytes`, as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The last part of an archive entry's name.
fn base_name(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

/// Take the app binary out of the archive into `staging`. The binary is the
/// entry named [`binary_name`], in any folder; an archive without one is an
/// error, so a saved error page is never installed.
pub fn extract_binary(archive: &str, bytes: &[u8], staging: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(staging)
        .map_err(|error| format!("the update folder could not be made: {error}"))?;
    let out = staging.join(binary_name());
    let write = |reader: &mut dyn std::io::Read| -> Result<(), String> {
        let mut file = std::fs::File::create(&out)
            .map_err(|error| format!("the update could not be written: {error}"))?;
        std::io::copy(reader, &mut file)
            .map_err(|error| format!("the update could not be written: {error}"))?;
        Ok(())
    };
    let mut found = false;
    if archive.ends_with(".zip") {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))
            .map_err(|error| format!("the download is not a zip archive: {error}"))?;
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index).map_err(|error| error.to_string())?;
            let matches = entry
                .name()
                .is_ok_and(|name| base_name(name.as_ref()) == binary_name());
            if matches {
                write(&mut entry)?;
                found = true;
                break;
            }
        }
    } else {
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
        let entries = tar
            .entries()
            .map_err(|error| format!("the download is not a tar.gz archive: {error}"))?;
        for entry in entries {
            let mut entry = entry.map_err(|error| error.to_string())?;
            let name = entry
                .path()
                .map_err(|error| error.to_string())?
                .to_string_lossy()
                .to_string();
            if base_name(&name) == binary_name() {
                write(&mut entry)?;
                found = true;
                break;
            }
        }
    }
    if !found {
        return Err("the downloaded archive does not contain Starpod".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o755))
            .map_err(|error| format!("the update could not be made executable: {error}"))?;
    }
    Ok(out)
}

/// Download one file from a release. `progress` gets the percentage done
/// whenever it changes, when the server says how big the file is.
async fn fetch(
    client: &reqwest::Client,
    url: &str,
    progress: &(dyn Fn(u8) + Send + Sync),
) -> Result<Vec<u8>, String> {
    let mut response = client
        .get(url)
        .header(USER_AGENT, format!("starpod/{CURRENT_VERSION}"))
        .send()
        .await
        .map_err(|error| format!("the download failed: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("the download failed: GitHub answered {status}"));
    }
    let total = response.content_length().filter(|total| *total > 0);
    let mut bytes = Vec::with_capacity(total.unwrap_or(0).min(256 << 20) as usize);
    let mut shown = None;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("the download failed: {error}"))?
    {
        bytes.extend_from_slice(&chunk);
        if let Some(total) = total {
            let percent = (bytes.len() as u64 * 100 / total).min(100) as u8;
            if shown != Some(percent) {
                shown = Some(percent);
                progress(percent);
            }
        }
    }
    Ok(bytes)
}

/// Download this platform's build of `release` into `staging`, check it, and
/// return the path of the new binary. `progress` gets the archive's
/// percentage done as it downloads.
pub async fn download_update(
    release: &Release,
    staging: &Path,
    progress: impl Fn(u8) + Send + Sync,
) -> Result<PathBuf, String> {
    let (archive_asset, checksum_asset) = pick_platform_assets(release)?;
    let archive = archive_asset.name.as_str();
    let client = starpod_subscription::tls::configure(reqwest::Client::builder())
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .map_err(|error| format!("the request could not be made: {error}"))?;
    let checksum = fetch(&client, &checksum_asset.url, &|_| {}).await?;
    let expected = parse_checksum(&String::from_utf8_lossy(&checksum))?;
    let bytes = fetch(&client, &archive_asset.url, &progress).await?;
    if sha256_hex(&bytes) != expected {
        return Err(
            "the download does not match the release's checksum; nothing was installed".to_string(),
        );
    }
    if cfg!(target_os = "android") {
        std::fs::create_dir_all(staging)
            .map_err(|error| format!("the download could not be saved: {error}"))?;
        let apk = staging.join(ANDROID_APK);
        std::fs::write(&apk, &bytes)
            .map_err(|error| format!("the download could not be saved: {error}"))?;
        return Ok(apk);
    }
    extract_binary(archive, &bytes, staging)
}

/// Hand the APK at `apk` to Android's installer.
pub fn install_apk(apk: &Path) -> Result<(), String> {
    let hooks = ANDROID.get().ok_or("this build cannot install an update")?;
    (hooks.install)(apk)
}

/// The path the running binary is moved to while the new one takes its place.
pub fn previous_binary_path(current: &Path) -> PathBuf {
    let mut name = current.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    current.with_file_name(name)
}

/// Put the binary at `new` in place of `current`, keeping the old one beside
/// it as `<name>.old` until the next start (see [`remove_previous_binary`]).
///
/// A running binary cannot be overwritten on Windows, but it can be renamed,
/// so the old one is renamed away first. If the new one cannot be put in
/// place, the old one is renamed back.
pub fn replace_binary(current: &Path, new: &Path) -> Result<(), String> {
    let previous = previous_binary_path(current);
    let _ = std::fs::remove_file(&previous);
    std::fs::rename(current, &previous)
        .map_err(|error| format!("the running Starpod could not be moved aside: {error}"))?;
    // A copy, not a rename: the download may be on another drive.
    if let Err(error) = std::fs::copy(new, current) {
        let _ = std::fs::rename(&previous, current);
        return Err(format!(
            "the new Starpod could not be put in place: {error}"
        ));
    }
    let _ = std::fs::remove_file(new);
    Ok(())
}

/// The binary to start once this one has shut down, set by a finished update.
static RELAUNCH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Start `binary` when this process exits (see [`relaunch_if_scheduled`]).
pub fn schedule_relaunch(binary: PathBuf) {
    let _ = RELAUNCH.set(binary);
}

/// Start the binary a finished update scheduled. Called at the very end of
/// `main`, after the single-instance channel is closed, so the new process
/// becomes the running instance instead of handing itself back to this one.
pub fn relaunch_if_scheduled() {
    if let Some(binary) = RELAUNCH.get() {
        match std::process::Command::new(binary).spawn() {
            Ok(_) => tracing::info!("started the updated Starpod"),
            Err(error) => tracing::warn!(%error, "could not start the updated Starpod"),
        }
    }
}

/// Remove the binary an update left beside the running one. Called at start;
/// by then the old binary is no longer running.
pub fn remove_previous_binary() {
    if let Ok(current) = std::env::current_exe() {
        let previous = previous_binary_path(&current);
        if previous.exists() {
            let _ = std::fs::remove_file(previous);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release_with(names: &[&str]) -> Release {
        Release {
            version: "0.3.0".into(),
            url: "https://example.test/r".into(),
            assets: names
                .iter()
                .map(|name| ReleaseAsset {
                    name: name.to_string(),
                    url: format!("https://example.test/{name}"),
                })
                .collect(),
        }
    }

    #[test]
    fn the_archive_and_its_checksum_are_picked_by_name() {
        let release = release_with(&["a.zip", "a.zip.sha256", "b.tar.gz", "b.tar.gz.sha256"]);
        let (archive, checksum) = pick_assets(&release, "b.tar.gz").unwrap();
        assert_eq!(archive.name, "b.tar.gz");
        assert_eq!(checksum.name, "b.tar.gz.sha256");
        assert!(pick_assets(&release, "c.zip").is_err(), "no build");
        let unchecked = release_with(&["a.zip"]);
        assert!(
            pick_assets(&unchecked, "a.zip").is_err(),
            "no checksum, no install"
        );
    }

    #[test]
    fn the_portable_archive_is_chosen_over_installers_and_legacy_names() {
        let (portable, legacy) = platform_archives().expect("a published platform");
        let setup = "starpod-x86_64-setup.exe";
        let with = |names: &[&str]| {
            let mut all: Vec<String> = Vec::new();
            for name in names {
                all.push(name.to_string());
                all.push(format!("{name}.sha256"));
            }
            let refs: Vec<&str> = all.iter().map(String::as_str).collect();
            release_with(&refs)
        };

        // The portable archive wins even when the legacy one is also there.
        let both = with(&[legacy, portable, setup]);
        let (archive, checksum) = pick_platform_assets(&both).unwrap();
        assert_eq!(archive.name, portable);
        assert_eq!(checksum.name, format!("{portable}.sha256"));

        // A release from before `-portable` is still installed from its name.
        let old = with(&[legacy]);
        assert_eq!(pick_platform_assets(&old).unwrap().0.name, legacy);

        // An installer alone is never an update.
        let installer_only = with(&[setup]);
        assert!(pick_platform_assets(&installer_only).is_err());
    }

    #[test]
    fn a_checksum_line_gives_its_hash() {
        let hash = "a".repeat(64);
        assert_eq!(parse_checksum(&format!("{hash}  starpod.zip\n")), Ok(hash));
        assert!(parse_checksum("").is_err());
        assert!(parse_checksum("not-a-hash file").is_err());
    }

    #[test]
    fn the_binary_is_taken_out_of_a_zip() {
        use std::io::Write as _;
        let mut bytes = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("starpod-x/README.md", options).unwrap();
            zip.write_all(b"readme").unwrap();
            zip.start_file(format!("starpod-x/{}", binary_name()), options)
                .unwrap();
            zip.write_all(b"new binary").unwrap();
            zip.finish().unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let out = extract_binary("x.zip", &bytes, dir.path()).unwrap();
        assert_eq!(std::fs::read(out).unwrap(), b"new binary");
        assert!(extract_binary("x.zip", b"<html>", dir.path()).is_err());
    }

    #[test]
    fn the_binary_is_taken_out_of_a_tar_gz() {
        let mut tarball = Vec::new();
        {
            let gz = flate2::write::GzEncoder::new(&mut tarball, flate2::Compression::fast());
            let mut builder = tar::Builder::new(gz);
            let data = b"new binary";
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(
                    &mut header,
                    format!("starpod-x/{}", binary_name()),
                    &data[..],
                )
                .unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let out = extract_binary("x.tar.gz", &tarball, dir.path()).unwrap();
        assert_eq!(std::fs::read(out).unwrap(), b"new binary");
    }

    #[test]
    fn the_binary_is_replaced_and_the_old_one_kept_aside() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join(binary_name());
        let new = dir.path().join("download").join(binary_name());
        std::fs::create_dir_all(new.parent().unwrap()).unwrap();
        std::fs::write(&current, b"old").unwrap();
        std::fs::write(&new, b"new").unwrap();
        replace_binary(&current, &new).unwrap();
        assert_eq!(std::fs::read(&current).unwrap(), b"new");
        assert_eq!(
            std::fs::read(previous_binary_path(&current)).unwrap(),
            b"old"
        );
    }

    /// A release served by a local stand-in for GitHub: this platform's
    /// archive holding `binary`, and a checksum file with `checksum`.
    async fn serve_release(
        binary: &[u8],
        checksum: Option<String>,
    ) -> (wiremock::MockServer, Release) {
        use std::io::Write as _;
        use wiremock::matchers::path;
        use wiremock::{Mock, ResponseTemplate};

        let archive = archive_name().expect("a published platform");
        let mut bytes = Vec::new();
        // The fixture uses the portable name, which is what the update picks.
        if archive.ends_with(".zip") {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
            zip.start_file(
                format!("starpod/{}", binary_name()),
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zip.write_all(binary).unwrap();
            zip.finish().unwrap();
        } else {
            let gz = flate2::write::GzEncoder::new(&mut bytes, flate2::Compression::fast());
            let mut builder = tar::Builder::new(gz);
            let mut header = tar::Header::new_gnu();
            header.set_size(binary.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, format!("starpod/{}", binary_name()), binary)
                .unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }
        let checksum = checksum.unwrap_or_else(|| format!("{}  {archive}\n", sha256_hex(&bytes)));

        let server = wiremock::MockServer::start().await;
        Mock::given(path(format!("/{archive}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
            .mount(&server)
            .await;
        Mock::given(path(format!("/{archive}.sha256")))
            .respond_with(ResponseTemplate::new(200).set_body_string(checksum))
            .mount(&server)
            .await;
        let release = Release {
            version: "9.9.9".into(),
            url: format!("{}/release", server.uri()),
            assets: [archive.to_string(), format!("{archive}.sha256")]
                .into_iter()
                .map(|name| ReleaseAsset {
                    url: format!("{}/{name}", server.uri()),
                    name,
                })
                .collect(),
        };
        (server, release)
    }

    #[tokio::test]
    async fn a_release_whose_checksum_matches_is_downloaded_and_unpacked() {
        let (_server, release) = serve_release(b"the new starpod", None).await;
        let dir = tempfile::tempdir().unwrap();
        let seen = std::sync::Mutex::new(Vec::new());
        let binary = download_update(&release, dir.path(), |percent| {
            seen.lock().unwrap().push(percent);
        })
        .await
        .expect("installs");
        assert_eq!(std::fs::read(binary).unwrap(), b"the new starpod");
        let seen = seen.into_inner().unwrap();
        assert_eq!(
            seen.last(),
            Some(&100),
            "the progress ends at 100%: {seen:?}"
        );
        assert!(seen.windows(2).all(|pair| pair[0] < pair[1]), "{seen:?}");
    }

    #[tokio::test]
    async fn a_release_whose_checksum_does_not_match_installs_nothing() {
        let wrong = format!("{}  starpod\n", "0".repeat(64));
        let (_server, release) = serve_release(b"the new starpod", Some(wrong)).await;
        let dir = tempfile::tempdir().unwrap();
        let error = download_update(&release, dir.path(), |_| {})
            .await
            .unwrap_err();
        assert!(error.contains("checksum"), "{error}");
        assert!(
            !dir.path().join(binary_name()).exists(),
            "nothing was written"
        );
    }

    #[test]
    fn a_release_body_lists_its_assets() {
        let body = r#"{
            "tag_name": "v0.3.1",
            "html_url": "https://github.com/foxgirlsorg/starpod/releases/tag/v0.3.1",
            "assets": [{"name": "starpod-windows-x86_64.zip",
                        "browser_download_url": "https://example.test/w.zip"}]
        }"#;
        let release = parse_release(body).unwrap();
        assert_eq!(release.assets.len(), 1);
        assert_eq!(release.assets[0].url, "https://example.test/w.zip");
    }

    #[test]
    fn a_leading_v_is_ignored_when_comparing() {
        assert_eq!(is_newer("v0.2.0", "0.1.0"), Ok(true));
        assert_eq!(is_newer("0.2.0", "v0.1.0"), Ok(true));
        assert_eq!(is_newer("v0.1.0", "0.1.0"), Ok(false));
    }

    #[test]
    fn an_older_or_equal_release_is_not_newer() {
        assert_eq!(is_newer("0.1.0", "0.2.0"), Ok(false));
        assert_eq!(is_newer("0.1.0", "0.1.0"), Ok(false));
    }

    #[test]
    fn each_part_is_compared_as_a_number() {
        assert_eq!(is_newer("0.10.0", "0.9.0"), Ok(true));
        assert_eq!(is_newer("1.0.0", "0.99.99"), Ok(true));
        assert_eq!(is_newer("0.1.1", "0.1.0"), Ok(true));
    }

    #[test]
    fn a_prerelease_is_older_than_its_release() {
        assert_eq!(is_newer("0.2.0-rc1", "0.1.0"), Ok(true));
        assert_eq!(is_newer("0.2.0-rc1", "0.2.0"), Ok(false));
        assert_eq!(is_newer("0.2.0", "0.2.0-rc1"), Ok(true));
    }

    #[test]
    fn a_tag_that_is_not_a_version_is_an_error() {
        assert!(is_newer("latest", "0.1.0").is_err());
        assert!(is_newer("v1.2", "0.1.0").is_err());
        assert!(is_newer("0.2.0", "not-a-version").is_err());
    }

    #[test]
    fn a_release_body_gives_its_version_and_page() {
        let body = r#"{
            "tag_name": "v0.3.1",
            "html_url": "https://github.com/foxgirlsorg/starpod/releases/tag/v0.3.1",
            "name": "ignored",
            "assets": []
        }"#;
        let release = parse_release(body).unwrap();
        assert_eq!(release.version, "0.3.1");
        assert_eq!(
            release.url,
            "https://github.com/foxgirlsorg/starpod/releases/tag/v0.3.1"
        );
    }

    #[test]
    fn a_release_body_without_a_tag_is_an_error() {
        assert!(parse_release(r#"{"html_url": "https://example.test"}"#).is_err());
        assert!(parse_release("not json").is_err());
    }

    #[test]
    fn a_store_or_package_manager_install_is_recognized() {
        let cases = [
            (
                r"C:\Users\a\scoop\apps\starpod\current\starpod.exe",
                false,
                Some("Scoop"),
            ),
            (
                r"C:\Users\a\AppData\Local\Microsoft\WinGet\Packages\starpod\starpod.exe",
                false,
                Some("WinGet"),
            ),
            (
                r"C:\Program Files\WindowsApps\Starpod_1.0\starpod.exe",
                false,
                Some("Microsoft Store"),
            ),
            (
                "/opt/homebrew/Cellar/starpod/1.0/bin/starpod",
                false,
                Some("Homebrew"),
            ),
            ("/nix/store/abc-starpod/bin/starpod", true, Some("Nix")),
            ("/usr/bin/starpod", true, Some("the system package manager")),
            // Starpod's own installer, the portable archive and a manual copy.
            (
                r"C:\Users\a\AppData\Local\Programs\Starpod\starpod.exe",
                false,
                None,
            ),
            ("/home/a/starpod/starpod", true, None),
            ("/usr/local/bin/starpod", true, None),
            ("/usr/bin/starpod", false, None),
        ];
        for (path, linux, expected) in cases {
            assert_eq!(package_manager_for(path, linux), expected, "{path}");
        }
    }

    #[test]
    fn only_a_direct_android_install_updates_itself() {
        for direct in [
            None,
            Some("com.android.shell"),
            Some("com.google.android.packageinstaller"),
            Some("com.android.packageinstaller"),
            Some("com.miui.packageinstaller"),
        ] {
            assert!(android_installed_directly(direct), "{direct:?}");
        }
        for store in [
            "org.fdroid.fdroid",
            "dev.imranr.obtainium",
            "com.android.vending",
            "com.aurora.store",
            "com.looker.droidify",
        ] {
            assert!(!android_installed_directly(Some(store)), "{store}");
        }
    }
}
