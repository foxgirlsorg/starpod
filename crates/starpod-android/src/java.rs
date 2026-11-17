//! The bridge to the app's Java code (android/java): the activity's methods
//! native code calls, and the native methods the activity calls back.

use std::ffi::c_void;
use std::sync::OnceLock;

use jni::objects::{GlobalRef, JObject, JString, JValue};
use jni::{JavaVM, NativeMethod};
use slint::android::AndroidApp;
use tracing::error;

/// The process's JavaVM and the activity, set once at start.
static JAVA: OnceLock<Java> = OnceLock::new();

/// The JavaVM and a global reference to the activity.
pub struct Java {
    vm: JavaVM,
    activity: GlobalRef,
}

/// Set up the bridge: keep the activity and register every native method
/// `MainActivity` declares.
pub fn init(app: &AndroidApp) -> Result<(), String> {
    // SAFETY: the pointers are the process's JavaVM and the activity object,
    // valid for as long as the activity is; the activity is held by a global
    // reference from here on.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }.map_err(|e| e.to_string())?;
    let mut env = vm
        .attach_current_thread_permanently()
        .map_err(|e| e.to_string())?;
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
    let global = env.new_global_ref(&activity).map_err(|e| e.to_string())?;
    let class = env.get_object_class(&activity).map_err(|e| e.to_string())?;
    let methods = [
        NativeMethod {
            name: "vpnReady".into(),
            sig: "(I)V".into(),
            fn_ptr: crate::vpn::vpn_ready as *mut c_void,
        },
        NativeMethod {
            name: "vpnFailed".into(),
            sig: "(Ljava/lang/String;)V".into(),
            fn_ptr: crate::vpn::vpn_failed as *mut c_void,
        },
        NativeMethod {
            name: "vpnPermission".into(),
            sig: "(ZLjava/lang/String;)V".into(),
            fn_ptr: crate::vpn::vpn_permission as *mut c_void,
        },
        NativeMethod {
            name: "updateFailed".into(),
            sig: "(Ljava/lang/String;)V".into(),
            fn_ptr: crate::update::update_failed as *mut c_void,
        },
    ];
    env.register_native_methods(&class, &methods)
        .map_err(|e| format!("the native methods could not be registered: {e}"))?;
    JAVA.set(Java {
        vm,
        activity: global,
    })
    .map_err(|_| "the Java bridge was set up twice".to_string())
}

/// The bridge, once [`init`] has run.
pub fn get() -> Option<&'static Java> {
    JAVA.get()
}

impl Java {
    /// Call one of the activity's `()V` methods. A failure is logged.
    pub fn call_void(&self, method: &str) {
        let outcome = self
            .vm
            .attach_current_thread_permanently()
            .and_then(|mut env| env.call_method(self.activity.as_obj(), method, "()V", &[]));
        if let Err(error) = outcome {
            error!(%error, method, "the Java call failed");
        }
    }

    /// Call one of the activity's methods that returns a `String` (or null),
    /// with at most one `String` argument.
    pub fn call_string(&self, method: &str, arg: Option<&str>) -> Result<Option<String>, String> {
        let mut env = self
            .vm
            .attach_current_thread_permanently()
            .map_err(|e| e.to_string())?;
        let value = match arg {
            Some(arg) => {
                let arg = env.new_string(arg).map_err(|e| e.to_string())?;
                env.call_method(
                    self.activity.as_obj(),
                    method,
                    "(Ljava/lang/String;)Ljava/lang/String;",
                    &[JValue::Object(&arg)],
                )
            }
            None => env.call_method(self.activity.as_obj(), method, "()Ljava/lang/String;", &[]),
        }
        .and_then(|value| value.l())
        .map_err(|e| format!("{method} failed: {e}"))?;
        if value.is_null() {
            return Ok(None);
        }
        let value = JString::from(value);
        let text: String = env.get_string(&value).map_err(|e| e.to_string())?.into();
        Ok(Some(text))
    }
}
