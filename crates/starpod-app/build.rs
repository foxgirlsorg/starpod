//! Embed the app icon in the Windows executable, so Explorer, the taskbar and
//! shortcuts show the Starpod mark. Other targets need nothing here.

fn main() {
    println!("cargo:rerun-if-changed=assets/starpod.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/starpod.ico");
        if let Err(error) = resource.compile() {
            // A missing resource compiler must not stop the build; the app
            // then simply has the default icon.
            println!("cargo:warning=the app icon was not embedded: {error}");
        }
    }
}
