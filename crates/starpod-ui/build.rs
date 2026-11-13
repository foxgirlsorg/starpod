fn main() {
    slint_build::compile("ui/design.slint").expect("the design tokens compile");
    slint_build::compile("ui/icons.slint").expect("the icon set compiles");
    slint_build::compile("ui/logo.slint").expect("the logo compiles");
    slint_build::compile("ui/primitives.slint").expect("the primitives compile");
}
