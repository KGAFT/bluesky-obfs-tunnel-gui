mod app;
mod engine;
mod platform;
mod profiles;

// Listener adapters and their regression tests come from the CLI; see docs/architecture.md.
mod http_proxy;
mod tcp_forward;
mod udp_forward;

slint::include_modules!();

pub fn run(data_dir: Option<std::path::PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    app::run(data_dir)
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: slint::android::AndroidApp) {
    let data_dir = app.internal_data_path();
    if let Err(error) = platform::init_android(&app) {
        eprintln!("Bluesky Tunnel platform initialization: {error}");
        return;
    }
    if let Err(error) = slint::android::init(app)
        .map_err(|error| -> Box<dyn std::error::Error> { error.into() })
        .and_then(|_| run(data_dir))
    {
        eprintln!("Bluesky Tunnel: {error}");
    }
}
