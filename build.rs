fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("ios") {
        cc::Build::new()
            .file("ios/Platform.m")
            .flag("-fobjc-arc")
            .flag("-fblocks")
            .compile("bluesky_ios_platform");
        println!("cargo:rustc-link-lib=framework=UIKit");
        println!("cargo:rustc-link-lib=framework=UniformTypeIdentifiers");
    }
    println!("cargo:rerun-if-changed=ios/Platform.m");
    slint_build::compile_with_config(
        "ui/app.slint",
        slint_build::CompilerConfiguration::new().with_style("fluent-dark".into()),
    )
    .expect("compile Slint interface");
}
