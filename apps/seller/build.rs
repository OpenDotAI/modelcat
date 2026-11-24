fn main() {
    #[cfg(feature = "desktop")]
    {
        println!("cargo:rerun-if-changed=icons/icon.png");
        println!("cargo:rerun-if-changed=icons/ModelCat.icns");
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
            println!("cargo:rerun-if-changed=src/quit_shortcut.m");
            cc::Build::new().file("src/quit_shortcut.m").flag("-fobjc-arc").compile("quit_shortcut");
        }
        tauri_build::build();
    }
}
