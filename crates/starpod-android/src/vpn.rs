//! The VPN: on a phone, the device's traffic reaches the engine through
//! Android's `VpnService` rather than a system proxy.
//!
//! The Java half (android/java) asks for the VPN permission and opens the
//! tunnel. This half reads the tunnel with tun2proxy, which runs a small
//! TCP/IP stack and hands each connection to the engine's local SOCKS port.
//! DNS queries in the tunnel get stand-in addresses, so the engine is given
//! each connection's host name, and its rules match by name as they do on
//! the desktop.
//!
//! [`Vpn`] is the service's [`PhoneTunnel`]: the service starts it once the
//! engine runs and stops it when the engine stops. A tunnel that fails or is
//! closed by the system stops the proxy and says why.

use std::sync::{Arc, Mutex, OnceLock};

use jni::JNIEnv;
use jni::objects::{JClass, JString};
use jni::sys::{jboolean, jint};
use starpod_app::{PhoneTunnel, SharedAppService};
use tokio::runtime::Handle;
use tokio::sync::oneshot;
use tracing::{info, warn};
use tun2proxy::{ArgDns, Args, CancellationToken, ProxyParameters};

/// The tunnel's MTU, as the Java side sets it.
const MTU: u16 = 1500;

/// The one [`Vpn`], for the native methods the Java side calls.
static VPN: OnceLock<Arc<Vpn>> = OnceLock::new();

/// The app's VPN.
pub struct Vpn {
    runtime: Handle,
    service: SharedAppService,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// The engine's SOCKS port while the VPN is wanted.
    port: Option<u16>,
    /// Stops the tun2proxy reading the tunnel, while one runs.
    running: Option<CancellationToken>,
    /// Waits for the user's answer to the VPN permission request.
    permission: Option<oneshot::Sender<Result<(), String>>>,
}

/// Make the VPN the service's phone tunnel. The Java bridge
/// ([`crate::java::init`]) must be set up first.
pub fn install(runtime: &Handle, service: SharedAppService) -> Result<(), String> {
    if crate::java::get().is_none() {
        return Err("the Java bridge is not set up".to_string());
    }
    let vpn = Arc::new(Vpn {
        runtime: runtime.clone(),
        service: service.clone(),
        state: Mutex::new(State::default()),
    });
    VPN.set(vpn.clone())
        .map_err(|_| "the VPN was set up twice".to_string())?;
    runtime.block_on(async move { service.lock().await.set_phone_tunnel(vpn) });
    Ok(())
}

impl Vpn {
    /// Call one of the activity's `()V` methods.
    fn call(&self, method: &str) {
        if let Some(java) = crate::java::get() {
            java.call_void(method);
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The tunnel is open: read it until the VPN stops.
    fn ready(&self, fd: i32) {
        let mut state = self.state();
        let Some(port) = state.port else {
            // Stopped while Android was opening it; the stop is on its way.
            return;
        };
        if let Some(old) = state.running.take() {
            old.cancel();
        }
        // tun2proxy reads its own copy of the descriptor, so the Java side
        // closing the tunnel never leaves it reading a reused number.
        // SAFETY: `fd` is the open tunnel the service just handed over.
        let own = unsafe { libc_dup(fd) };
        if own < 0 {
            drop(state);
            self.fail("The VPN tunnel could not be read".to_string());
            return;
        }
        let token = CancellationToken::new();
        state.running = Some(token.clone());
        drop(state);

        let proxy: ProxyParameters = match format!("socks5://127.0.0.1:{port}").parse() {
            Ok(proxy) => proxy,
            Err(error) => {
                self.fail(format!("The VPN could not start: {error}"));
                return;
            }
        };
        let mut args = Args::default();
        args.proxy(proxy)
            .dns(ArgDns::Virtual)
            .tun_fd(Some(own))
            .close_fd_on_drop(true)
            .setup(false);
        info!(port, "the VPN is up");
        let service = self.service.clone();
        self.runtime
            .spawn(async move { service.lock().await.phone_tunnel_ready() });
        self.runtime.spawn(async move {
            let outcome = tun2proxy::general_run_async(args, MTU, false, token.clone()).await;
            if token.is_cancelled() {
                return;
            }
            let reason = match outcome {
                Ok(_) => "The VPN stopped".to_string(),
                Err(error) => format!("The VPN stopped: {error}"),
            };
            if let Some(vpn) = VPN.get() {
                vpn.fail(reason);
            }
        });
    }

    /// The VPN failed or was closed: close what is left of it and stop the
    /// proxy, unless the VPN was no longer wanted anyway.
    fn fail(&self, reason: String) {
        let wanted = {
            let mut state = self.state();
            if let Some(running) = state.running.take() {
                running.cancel();
            }
            state.port.take().is_some()
        };
        if !wanted {
            return;
        }
        warn!(%reason, "the VPN failed");
        self.call("stopVpn");
        let service = self.service.clone();
        self.runtime.spawn(async move {
            service.lock().await.phone_tunnel_failed(&reason).await;
        });
    }
}

/// How long the user has to answer the VPN permission request.
const PERMISSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

#[async_trait::async_trait]
impl PhoneTunnel for Vpn {
    async fn prepare(&self) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.state().permission = Some(tx);
        self.call("requestVpnPermission");
        match tokio::time::timeout(PERMISSION_TIMEOUT, rx).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err("the VPN permission request was dropped".to_string()),
            Err(_) => Err("Starpod needs the VPN permission to connect".to_string()),
        }
    }

    fn start(&self, socks_port: u16) {
        {
            let mut state = self.state();
            state.port = Some(socks_port);
            if let Some(old) = state.running.take() {
                old.cancel();
            }
        }
        self.call("startVpn");
    }

    fn stop(&self) {
        {
            let mut state = self.state();
            state.port = None;
            if let Some(running) = state.running.take() {
                running.cancel();
            }
        }
        self.call("stopVpn");
    }
}

unsafe extern "C" {
    #[link_name = "dup"]
    fn libc_dup(fd: i32) -> i32;
}

/// `MainActivity.vpnReady(int)`.
pub extern "system" fn vpn_ready(_env: JNIEnv, _class: JClass, fd: jint) {
    if let Some(vpn) = VPN.get() {
        vpn.ready(fd);
    }
}

/// `MainActivity.vpnFailed(String)`.
pub extern "system" fn vpn_failed(mut env: JNIEnv, _class: JClass, reason: JString) {
    let reason: String = env
        .get_string(&reason)
        .map(Into::into)
        .unwrap_or_else(|_| "The VPN stopped".to_string());
    if let Some(vpn) = VPN.get() {
        vpn.fail(reason);
    }
}

/// `MainActivity.vpnPermission(boolean, String)`.
pub extern "system" fn vpn_permission(
    mut env: JNIEnv,
    _class: JClass,
    granted: jboolean,
    reason: JString,
) {
    let answer = if granted != 0 {
        Ok(())
    } else {
        Err(env
            .get_string(&reason)
            .map(Into::into)
            .unwrap_or_else(|_| "Starpod needs the VPN permission to connect".to_string()))
    };
    if let Some(vpn) = VPN.get()
        && let Some(waiting) = vpn.state().permission.take()
    {
        let _ = waiting.send(answer);
    }
}
