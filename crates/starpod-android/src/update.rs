//! The app's own updates on Android: the shared update code downloads and
//! checks the release's APK, and this hands it to Android's installer
//! (`MainActivity.installApk`), which asks the user to confirm.
//!
//! Only an app installed straight from an APK updates itself. One that a
//! store or an updater installed (F-Droid, Obtainium, Google Play, …) is
//! updated by it, so Starpod hides its update controls there; see
//! `starpod_app::update::package_manager`.

use jni::JNIEnv;
use jni::objects::{JClass, JString};
use starpod_app::update::{self, AndroidHooks};
use tracing::{info, warn};

/// Tell the update code who installed the app and how to install an APK.
pub fn init() {
    let installer = crate::java::get().and_then(|java| {
        java.call_string("installerPackage", None)
            .unwrap_or_else(|error| {
                warn!(%error, "the installer could not be read");
                None
            })
    });
    info!(
        installer = installer.as_deref().unwrap_or("none"),
        "installed by"
    );
    update::set_android_hooks(AndroidHooks {
        installer,
        install: Box::new(|apk| {
            let java = crate::java::get().ok_or("this build cannot install an update")?;
            let path = apk.to_string_lossy();
            match java.call_string("installApk", Some(&path))? {
                None => Ok(()),
                Some(error) => Err(error),
            }
        }),
    });
}

/// `MainActivity.updateFailed(String)`: Android did not install the update.
pub extern "system" fn update_failed(mut env: JNIEnv, _class: JClass, reason: JString) {
    let reason: String = env
        .get_string(&reason)
        .map(Into::into)
        .unwrap_or_else(|_| "Android did not install the update".to_string());
    warn!(%reason, "the update was not installed");
    starpod_app::ui::runtime::show_error(reason);
}
