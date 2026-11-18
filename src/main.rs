fn main() {
    if let Err(error) = bluesky_obfs_t_gui::run(None) {
        eprintln!("Bluesky Tunnel: {error}");
        std::process::exit(1);
    }
}
