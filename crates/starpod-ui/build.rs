fn main() {
    // The translations are bundled into the binary: one `.po` per language under
    // `translations/<lang>/LC_MESSAGES/starpod-ui.po` (the domain is the crate name).
    // Messages carry no context, so `@tr("...")` keys on the text alone.
    let config = slint_build::CompilerConfiguration::new()
        .with_bundled_translations("translations")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("ui/app.slint", config).expect("the Starpod UI compiles");
    println!("cargo:rerun-if-changed=translations");
}
