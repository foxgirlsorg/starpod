//! The Starpod desktop app.
//!
//! The tokio runtime runs on its own worker threads and owns the service.
//! The Slint event loop runs on the main thread (ARCHITECTURE §7). The two
//! meet only through `slint::invoke_from_event_loop` and the callbacks.

use std::error::Error;
use std::time::Duration;

use starpod_app::{AppError, AppService, SharedAppService, ui};
use tracing_subscriber::EnvFilter;

/// How long a panic waits for the engine to stop before the process ends.
const PANIC_STOP_TIMEOUT: Duration = Duration::from_secs(5);

fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("starpod-runtime")
        .build()?;
    let service = runtime.block_on(open_service())?;
    install_panic_hook(runtime.handle().clone(), service.clone());

    let outcome = ui::run(runtime.handle().clone(), service);
    runtime.shutdown_timeout(Duration::from_secs(5));
    outcome
}

/// Open the app's real dependencies and load the saved state.
async fn open_service() -> Result<SharedAppService, AppError> {
    let mut service = AppService::open_default()?;
    service.load().await?;
    Ok(service.into_shared())
}

/// Stop the engine when the app panics, so a crash does not leave the proxy
/// orphaned. The previous hook still runs afterwards.
fn install_panic_hook(handle: tokio::runtime::Handle, service: SharedAppService) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        stop_engine_after_panic(&handle, &service);
        previous(info);
    }));
}

/// Run the graceful stop on a helper thread. The panicking thread may be the
/// runtime's own, where blocking is not allowed, and it may hold the service
/// lock, so both the lock and the stop are bounded by a timeout.
fn stop_engine_after_panic(handle: &tokio::runtime::Handle, service: &SharedAppService) {
    let (finished, done) = std::sync::mpsc::channel();
    let handle = handle.clone();
    let service = service.clone();
    std::thread::spawn(move || {
        let stop = handle.block_on(async {
            tokio::time::timeout(PANIC_STOP_TIMEOUT, async {
                service.lock().await.disconnect().await
            })
            .await
        });
        match stop {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(%error, "the engine did not stop after a panic"),
            Err(_) => tracing::warn!("timed out stopping the engine after a panic"),
        }
        let _ = finished.send(());
    });
    if done
        .recv_timeout(PANIC_STOP_TIMEOUT + Duration::from_secs(1))
        .is_err()
    {
        tracing::warn!("the engine stop after a panic did not finish in time");
    }
}
