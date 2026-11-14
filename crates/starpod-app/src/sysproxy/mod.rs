//! The operating system's proxy setting, pointed at the running engine.
//!
//! The service drives this module at three points: when the engine starts or
//! its proxy settings change ([`SystemProxy::apply`]), when the engine stops
//! or the app quits ([`SystemProxy::release`]), and at startup
//! ([`recover`]). Before a value is changed, the values being replaced are
//! written to a record file. A crash therefore leaves the record behind, and
//! the next start restores the user's settings from it.
//!
//! The platform code lives in one file per operating system. Each file
//! implements [`Backend`] with no shell: Windows uses the registry and
//! WinINet, macOS uses `networksetup`, Linux uses `gsettings` and KDE's
//! `kwriteconfig`.

pub mod pac;

#[cfg(target_os = "macos")]
#[path = "mac.rs"]
mod platform;

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod platform;

#[cfg(windows)]
#[path = "win.rs"]
mod platform;

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
#[path = "other.rs"]
mod platform;

#[cfg(windows)]
pub use platform::set_run_value;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use starpod_core::domain::SystemProxyMode;

use self::pac::PacServer;

/// The record file's name, in the app's data directory.
pub const RECORD_FILE: &str = "sysproxy-state.json";

/// The record format version. A record with another version is not trusted.
const RECORD_VERSION: u32 = 1;

/// A failure to read, change or restore the system proxy. The message is
/// safe to show to the user.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SysproxyError(pub String);

impl From<io::Error> for SysproxyError {
    fn from(error: io::Error) -> Self {
        Self(error.to_string())
    }
}

/// One operating-system setting Starpod read or changed.
///
/// `location` and `key` are interpreted by the platform that wrote them: a
/// registry subkey and value name on Windows, a service name and key on macOS,
/// a schema or file group and key on Linux. `value` is `None` when the setting
/// did not exist, so restoring it removes the setting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSetting {
    pub location: String,
    pub key: String,
    pub value: Option<String>,
}

/// The record file's contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub version: u32,
    /// The backend that wrote the record. Only the same backend restores it.
    pub backend: String,
    pub settings: Vec<SavedSetting>,
}

/// What the proxy settings should be. Two equal values mean nothing changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Desired {
    pub mode: SystemProxyMode,
    /// The engine's mixed port.
    pub port: u16,
    /// The bypass list in the platform's own syntax.
    pub bypass: String,
    /// The PAC script, before its port is filled in.
    pub pac_script: String,
}

/// What a backend is asked to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub mode: SystemProxyMode,
    pub port: u16,
    pub bypass: String,
    /// The URL of the PAC script, in PAC mode.
    pub pac_url: Option<String>,
}

/// The platform's proxy settings.
///
/// Every method is synchronous and runs no shell. Failures that do not stop
/// the change come back as warnings; a failure that does is an error.
pub trait Backend: Send + Sync {
    /// The backend's name, written into the record.
    fn name(&self) -> &'static str;
    /// Read the settings [`Backend::apply`] changes.
    fn capture(&self) -> Result<Vec<SavedSetting>, SysproxyError>;
    /// Write the plan.
    fn apply(&self, plan: &Plan) -> Result<Vec<String>, SysproxyError>;
    /// Write saved settings back. A setting with no value is removed.
    fn restore(&self, saved: &[SavedSetting]) -> Result<Vec<String>, SysproxyError>;
}

/// The backend for the operating system this build runs on.
pub fn os_backend() -> Box<dyn Backend> {
    platform::backend()
}

/// The system proxy as Starpod has set it.
#[derive(Debug, Default)]
pub struct SystemProxy {
    /// What is in effect now. `None` when Starpod has not changed the proxy.
    applied: Option<Desired>,
    /// The PAC server, in PAC mode.
    pac: Option<PacServer>,
    /// The last failure to change the proxy, if it is still unresolved.
    pub error: Option<String>,
}

impl SystemProxy {
    /// Whether Starpod's proxy settings are in effect.
    pub fn is_active(&self) -> bool {
        self.applied.is_some()
    }

    /// Point the system proxy at the engine as `want` describes.
    ///
    /// Does nothing when `want` is already in effect. The record is written
    /// before anything changes, and only when there is none yet, so a second
    /// change never records Starpod's own settings as the user's. A failed
    /// change is rolled back before the error is returned.
    pub async fn apply(
        &mut self,
        backend: &dyn Backend,
        record_path: &Path,
        want: Desired,
    ) -> Result<Vec<String>, SysproxyError> {
        if self.applied.as_ref() == Some(&want) {
            return Ok(Vec::new());
        }
        if !record_path.exists() {
            let settings = backend.capture()?;
            write_record(
                record_path,
                &Record {
                    version: RECORD_VERSION,
                    backend: backend.name().to_string(),
                    settings,
                },
            )?;
        }
        self.pac = None;
        let pac_url = match want.mode {
            SystemProxyMode::Manual => None,
            SystemProxyMode::Pac => {
                let body = pac::render(&want.pac_script, want.port);
                let server = PacServer::start(body).await?;
                let url = server.url();
                self.pac = Some(server);
                Some(url)
            }
        };
        let plan = Plan {
            mode: want.mode,
            port: want.port,
            bypass: want.bypass.clone(),
            pac_url,
        };
        match backend.apply(&plan) {
            Ok(warnings) => {
                self.applied = Some(want);
                self.error = None;
                Ok(warnings)
            }
            Err(error) => {
                // Put back whatever was written before the failure. A failure
                // here keeps the record, so the next start still restores it.
                let _ = self.release(backend, record_path);
                Err(error)
            }
        }
    }

    /// Restore the settings the record holds, and remove the record. Does
    /// nothing when there is no record. If the restore fails, the record is
    /// kept so the next start can try again.
    pub fn release(
        &mut self,
        backend: &dyn Backend,
        record_path: &Path,
    ) -> Result<Vec<String>, SysproxyError> {
        self.pac = None;
        self.applied = None;
        let Some(record) = read_record(record_path)? else {
            return Ok(Vec::new());
        };
        if record.backend != backend.name() {
            return Err(SysproxyError(format!(
                "the saved proxy settings were written by {}, which this system does not use",
                record.backend
            )));
        }
        let warnings = backend.restore(&record.settings)?;
        remove_record(record_path)?;
        Ok(warnings)
    }
}

/// Restore the settings left behind by an earlier run that did not exit
/// cleanly. Called at startup, and only when no engine is running.
pub fn recover(backend: &dyn Backend, record_path: &Path) -> Result<Vec<String>, SysproxyError> {
    SystemProxy::default().release(backend, record_path)
}

/// Write the record atomically: a temporary file, then a rename.
pub fn write_record(path: &Path, record: &Record) -> Result<(), SysproxyError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text =
        serde_json::to_string_pretty(record).map_err(|error| SysproxyError(error.to_string()))?;
    let temporary: PathBuf = path.with_extension("json.tmp");
    fs::write(&temporary, text)?;
    fs::rename(&temporary, path)?;
    Ok(())
}

/// Read the record, if there is one. A record that cannot be parsed, or that
/// has another version, is an error, so it is never silently discarded.
pub fn read_record(path: &Path) -> Result<Option<Record>, SysproxyError> {
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path)?;
    let record: Record = serde_json::from_str(&text).map_err(|error| {
        SysproxyError(format!("the saved proxy settings cannot be read: {error}"))
    })?;
    if record.version != RECORD_VERSION {
        return Err(SysproxyError(format!(
            "the saved proxy settings have version {} and this build reads version {RECORD_VERSION}",
            record.version
        )));
    }
    Ok(Some(record))
}

fn remove_record(path: &Path) -> Result<(), SysproxyError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// A backend that keeps the proxy settings in memory.
    struct Fake {
        settings: Mutex<Vec<SavedSetting>>,
        fail_apply: bool,
    }

    impl Fake {
        fn with(settings: Vec<SavedSetting>) -> Self {
            Self {
                settings: Mutex::new(settings),
                fail_apply: false,
            }
        }

        fn value(&self, key: &str) -> Option<String> {
            self.settings
                .lock()
                .unwrap()
                .iter()
                .find(|setting| setting.key == key)
                .and_then(|setting| setting.value.clone())
        }
    }

    impl Backend for Fake {
        fn name(&self) -> &'static str {
            "fake"
        }

        fn capture(&self) -> Result<Vec<SavedSetting>, SysproxyError> {
            Ok(self.settings.lock().unwrap().clone())
        }

        fn apply(&self, plan: &Plan) -> Result<Vec<String>, SysproxyError> {
            if self.fail_apply {
                return Err(SysproxyError("refused".into()));
            }
            let mut settings = self.settings.lock().unwrap();
            set(
                &mut settings,
                "server",
                Some(format!("127.0.0.1:{}", plan.port)),
            );
            set(&mut settings, "pac", plan.pac_url.clone());
            Ok(Vec::new())
        }

        fn restore(&self, saved: &[SavedSetting]) -> Result<Vec<String>, SysproxyError> {
            *self.settings.lock().unwrap() = saved.to_vec();
            Ok(Vec::new())
        }
    }

    fn set(settings: &mut Vec<SavedSetting>, key: &str, value: Option<String>) {
        match settings.iter_mut().find(|setting| setting.key == key) {
            Some(setting) => setting.value = value,
            None => settings.push(SavedSetting {
                location: "test".into(),
                key: key.into(),
                value,
            }),
        }
    }

    fn manual(port: u16) -> Desired {
        Desired {
            mode: SystemProxyMode::Manual,
            port,
            bypass: "localhost".into(),
            pac_script: String::new(),
        }
    }

    fn user_settings() -> Vec<SavedSetting> {
        vec![SavedSetting {
            location: "test".into(),
            key: "server".into(),
            value: Some("proxy.corp:3128".into()),
        }]
    }

    fn record_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join(RECORD_FILE)
    }

    #[test]
    fn the_default_bypass_list_skips_loopback_and_private_networks() {
        let bypass = starpod_core::domain::default_sysproxy_bypass();
        for entry in ["localhost", "127.", "192.168.", "10.", "172.16."] {
            assert!(bypass.contains(entry), "{entry} missing from {bypass}");
        }
        if cfg!(windows) {
            assert!(bypass.contains("<local>"));
            assert!(bypass.split(';').count() > 2);
        } else {
            assert!(bypass.contains("::1"));
            assert!(bypass.split(',').count() > 2);
        }
    }

    #[tokio::test]
    async fn apply_records_the_previous_values_and_release_restores_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = record_path(&dir);
        let backend = Fake::with(user_settings());
        let mut proxy = SystemProxy::default();

        proxy.apply(&backend, &path, manual(7890)).await.unwrap();
        assert!(proxy.is_active());
        assert_eq!(backend.value("server").as_deref(), Some("127.0.0.1:7890"));
        assert!(path.exists(), "the record is written before the change");

        proxy.release(&backend, &path).unwrap();
        assert!(!proxy.is_active());
        assert_eq!(backend.value("server").as_deref(), Some("proxy.corp:3128"));
        assert!(!path.exists(), "a restored record is removed");
    }

    #[tokio::test]
    async fn a_second_apply_keeps_the_first_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = record_path(&dir);
        let backend = Fake::with(user_settings());
        let mut proxy = SystemProxy::default();

        proxy.apply(&backend, &path, manual(7890)).await.unwrap();
        proxy.apply(&backend, &path, manual(7891)).await.unwrap();
        proxy.release(&backend, &path).unwrap();

        assert_eq!(backend.value("server").as_deref(), Some("proxy.corp:3128"));
    }

    #[tokio::test]
    async fn a_crash_leaves_a_record_that_startup_recovery_restores() {
        let dir = tempfile::tempdir().unwrap();
        let path = record_path(&dir);
        let backend = Fake::with(user_settings());
        let mut proxy = SystemProxy::default();
        proxy.apply(&backend, &path, manual(7890)).await.unwrap();

        // The process ends here, without release. A new run starts with
        // nothing applied and finds the record.
        drop(proxy);
        let warnings = recover(&backend, &path).unwrap();

        assert!(warnings.is_empty());
        assert_eq!(backend.value("server").as_deref(), Some("proxy.corp:3128"));
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn a_failed_apply_rolls_back_and_reports_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = record_path(&dir);
        let mut backend = Fake::with(user_settings());
        backend.fail_apply = true;
        let mut proxy = SystemProxy::default();

        let error = proxy
            .apply(&backend, &path, manual(7890))
            .await
            .unwrap_err();

        assert_eq!(error.0, "refused");
        assert!(!proxy.is_active());
        assert!(!path.exists());
    }

    #[test]
    fn a_record_round_trips_through_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = record_path(&dir);
        let record = Record {
            version: RECORD_VERSION,
            backend: "fake".into(),
            settings: vec![
                SavedSetting {
                    location: "HKCU\\Software\\Example".into(),
                    key: "ProxyEnable".into(),
                    value: Some("1".into()),
                },
                SavedSetting {
                    location: "HKCU\\Software\\Example".into(),
                    key: "AutoConfigURL".into(),
                    value: None,
                },
            ],
        };

        write_record(&path, &record).unwrap();
        let read = read_record(&path).unwrap();

        assert_eq!(read, Some(record));
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn a_missing_record_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_record(&record_path(&dir)).unwrap(), None);
    }

    #[test]
    fn a_record_of_another_version_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = record_path(&dir);
        fs::write(
            &path,
            r#"{"version": 99, "backend": "fake", "settings": []}"#,
        )
        .unwrap();

        let error = read_record(&path).unwrap_err();

        assert!(error.0.contains("version 99"));
        assert!(path.exists(), "an unreadable record is kept, not discarded");
    }
}
