//! Render the real Slint UI at desktop and phone sizes without a display server.
use bluesky_obfs_t_gui::AppWindow;
use slint::{
    ComponentHandle,
    platform::{
        Platform, WindowAdapter,
        software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    },
};
use std::{path::Path, rc::Rc};

struct PreviewPlatform(Rc<MinimalSoftwareWindow>);
impl Platform for PreviewPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

fn snapshot(
    ui: &AppWindow,
    window: &MinimalSoftwareWindow,
    dir: &Path,
    name: &str,
    width: u32,
    height: u32,
) {
    ui.window()
        .set_size(slint::PhysicalSize::new(width, height));
    assert_eq!(ui.get_compact(), width < 760);
    let mut pixels = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(width, height);
    ui.window().request_redraw();
    assert!(window.draw_if_needed(|renderer| {
        renderer.render(pixels.make_mut_slice(), width as usize);
    }));
    let file = std::fs::File::create(dir.join(name)).unwrap();
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(pixels.as_bytes())
        .unwrap();
}

#[test]
fn render_desktop_and_mobile_layouts() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(PreviewPlatform(window.clone()))).unwrap();
    let ui = AppWindow::new().unwrap();
    ui.set_profile_names(Rc::new(slint::VecModel::from(vec!["My tunnel".into()])).into());
    ui.set_config_json("{\n  \"server_address\": \"127.0.0.1:443\"\n}".into());
    ui.set_activity("00:00:00  Client ready.\n\nConnection events appear here.".into());
    ui.set_background_info("The tunnel stays active while the application is running.".into());
    ui.show().unwrap();
    let dir = std::env::var_os("BLUESKY_PREVIEW_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "target/previews".into());
    std::fs::create_dir_all(&dir).unwrap();
    snapshot(&ui, &window, &dir, "desktop.png", 1180, 820);
    ui.set_mobile_import(true);
    ui.set_android(true);
    ui.set_background_info("A foreground service keeps the Android tunnel running. Set battery usage to Unrestricted in Android settings.".into());
    snapshot(&ui, &window, &dir, "mobile.png", 390, 844);
    snapshot(&ui, &window, &dir, "small-phone.png", 360, 640);
    ui.set_page(1);
    snapshot(&ui, &window, &dir, "mobile-profile.png", 390, 844);
    ui.set_page(3);
    snapshot(&ui, &window, &dir, "mobile-settings.png", 390, 844);
    ui.hide().unwrap();
}
