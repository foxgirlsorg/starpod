//! The engine's install location must be the app's own data directory.
//!
//! `starpod-engine` cannot see `starpod-store`, so it repeats the rule
//! "platform local data root plus `Starpod`". This test is what keeps the two
//! from drifting: if `AppPaths` moves, or the engine computes its own path,
//! it fails.

use starpod_engine::download;
use starpod_store::AppPaths;

#[test]
fn the_default_engine_location_is_under_the_app_data_directory() {
    let paths = AppPaths::resolve().expect("the platform directories resolve");
    let data_dir = paths.data_dir().to_path_buf();

    assert_eq!(download::default_data_dir(), data_dir);
    assert_eq!(
        download::install_dir_in(&data_dir),
        data_dir.join("engine").join(download::ENGINE_VERSION)
    );
    assert_eq!(
        download::installed_binary_path(),
        download::installed_binary_path_in(&data_dir)
    );
    assert_eq!(
        download::is_installed(),
        download::is_installed_in(&data_dir)
    );
}
