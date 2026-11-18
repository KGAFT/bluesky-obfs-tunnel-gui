use bluesky_obfs_tunnel::config::ClientSettings;
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub enum Mode {
    #[default]
    Http,
    Tcp,
    Udp,
}

impl Mode {
    pub fn index(self) -> i32 {
        match self {
            Self::Http => 0,
            Self::Tcp => 1,
            Self::Udp => 2,
        }
    }

    pub fn from_index(index: i32) -> Self {
        match index {
            1 => Self::Tcp,
            2 => Self::Udp,
            _ => Self::Http,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Profile {
    pub name: String,
    pub mode: Mode,
    pub destination: String,
    pub remember_password: bool,
    pub settings: ClientSettings,
}

impl Default for Profile {
    fn default() -> Self {
        let mut settings = ClientSettings::default();
        settings.client_identity.clear();
        settings.client_password.clear();
        Self {
            name: "My tunnel".into(),
            mode: Mode::Http,
            destination: "127.0.0.1:8090".into(),
            remember_password: false,
            settings,
        }
    }
}

impl Profile {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Give this profile a name.".into());
        }
        self.settings
            .validate()
            .map_err(|errors| errors.join("; "))?;
        let bind: std::net::SocketAddr = self
            .settings
            .local_proxy_bind
            .parse()
            .map_err(|_| "Invalid listener address")?;
        if bind.port() == 0 {
            return Err("Choose a nonzero listener port.".into());
        }
        if self.mode != Mode::Http {
            // Use the library's host/port validator without resolving remote DNS locally.
            let mut destination_check = self.settings.clone();
            destination_check.server_address = self.destination.clone();
            destination_check
                .validate()
                .map_err(|_| "Destination must be host:port; use brackets for IPv6.".to_string())?;
        }
        Ok(())
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Profiles {
    pub selected: usize,
    pub profiles: Vec<Profile>,
}

// Activity recreation must not be treated as a process restart: passwords the
// user chose to keep only in memory remain available for this process's lifetime.
#[cfg(any(target_os = "android", target_os = "ios"))]
static SESSION: std::sync::Mutex<Option<(PathBuf, Profiles)>> = std::sync::Mutex::new(None);

pub fn cache_session(dir: &Path, profiles: &Profiles) {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        *SESSION.lock().unwrap() = Some((dir.to_path_buf(), profiles.clone()));
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    let _ = (dir, profiles);
}

pub fn load_session(dir: &Path) -> io::Result<Profiles> {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    if let Some((cached_dir, profiles)) = SESSION.lock().unwrap().as_ref()
        && cached_dir == dir
    {
        return Ok(profiles.clone());
    }
    let profiles = load(dir)?;
    cache_session(dir, &profiles);
    Ok(profiles)
}

pub fn data_dir() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("BLUESKY_GUI_DATA_DIR") {
        return Ok(path.into());
    }
    #[cfg(not(target_os = "windows"))]
    let home = std::env::var_os("HOME").map(PathBuf::from);
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let base = home.map(|path| path.join("Library/Application Support"));
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "ios")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.map(|path| path.join(".config")));
    base.map(|path| path.join("bluesky-tunnel"))
        .ok_or_else(|| io::Error::other("Cannot locate app storage; set BLUESKY_GUI_DATA_DIR"))
}

pub fn prepare(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    // Embed the same captured pair shipped by the terminal client, for mobile sandboxes too.
    for (name, bytes) in [
        (
            "cli_pattern.bin",
            include_bytes!("../../bluesky-obfs-tunnel/cli_pattern.bin").as_slice(),
        ),
        (
            "serv_pattern.bin",
            include_bytes!("../../bluesky-obfs-tunnel/serv_pattern.bin").as_slice(),
        ),
    ] {
        let path = dir.join(name);
        if !path.exists() {
            atomic_write(&path, bytes)?;
        }
    }
    Ok(())
}

pub fn load(dir: &Path) -> io::Result<Profiles> {
    match fs::read(dir.join("profiles.json")) {
        Ok(bytes) => {
            let mut profiles: Profiles = serde_json::from_slice(&bytes)?;
            if profiles.profiles.is_empty() {
                return Err(io::Error::other("Profile file has no profiles"));
            }
            profiles.selected = profiles.selected.min(profiles.profiles.len() - 1);
            Ok(profiles)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Profiles {
            selected: 0,
            profiles: vec![Profile::default()],
        }),
        Err(error) => Err(error),
    }
}

pub fn save(dir: &Path, profiles: &Profiles) -> io::Result<()> {
    let mut persisted = profiles.clone();
    for profile in &mut persisted.profiles {
        if !profile.remember_password {
            profile.settings.client_password.clear();
        }
    }
    atomic_write(
        &dir.join("profiles.json"),
        &serde_json::to_vec_pretty(&persisted)?,
    )?;
    cache_session(dir, profiles);
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    // NamedTempFile is private (0600 on Unix), and persist replaces atomically.
    let mut file = tempfile::NamedTempFile::new_in(
        path.parent()
            .ok_or_else(|| io::Error::other("Missing parent directory"))?,
    )?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    // Persist the rename as well as the contents before reporting success.
    #[cfg(unix)]
    fs::File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}

pub const MAX_PATTERN_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy)]
pub enum PatternKind {
    Client,
    Server,
}

impl PatternKind {
    pub fn path_mut(self, profile: &mut Profile) -> &mut String {
        match self {
            Self::Client => &mut profile.settings.patterns.client_pattern_path,
            Self::Server => &mut profile.settings.patterns.server_pattern_path,
        }
    }
}

/// Validate using the protocol library before publishing an immutable private copy.
/// Unique names keep other profiles and the previously selected pair intact.
pub fn import_pattern(dir: &Path, source: &Path, kind: PatternKind) -> io::Result<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(source)?
        .take(MAX_PATTERN_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_PATTERN_BYTES {
        return Err(io::Error::other("Pattern must contain 1 byte to 16 MiB."));
    }
    let staging = tempfile::tempdir_in(dir)?;
    prepare(staging.path())?;
    let name = match kind {
        PatternKind::Client => "cli_pattern.bin",
        PatternKind::Server => "serv_pattern.bin",
    };
    fs::write(staging.path().join(name), &bytes)?;
    let mut validation = Profile::default();
    resolve_patterns(&mut validation, staging.path());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let pair = runtime.block_on(validation.settings.patterns.load())?;
    let pattern = match kind {
        PatternKind::Client => pair.client,
        PatternKind::Server => pair.server,
    };
    if pattern.known_packet_sizes().is_empty() {
        return Err(io::Error::other("Pattern contains no captured packets."));
    }
    let imports = dir.join("patterns");
    fs::create_dir_all(&imports)?;
    let mut file = tempfile::Builder::new()
        .prefix(name)
        .suffix(".bin")
        .tempfile_in(&imports)?;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    let (_, path) = file.keep().map_err(|error| error.error)?;
    #[cfg(unix)]
    fs::File::open(&imports)?.sync_all()?;
    Ok(path
        .strip_prefix(dir)
        .unwrap()
        .to_string_lossy()
        .into_owned())
}

pub fn resolve_patterns(profile: &mut Profile, dir: &Path) {
    for value in [
        &mut profile.settings.patterns.client_pattern_path,
        &mut profile.settings.patterns.server_pattern_path,
    ] {
        if Path::new(value).is_relative() {
            *value = dir.join(&*value).to_string_lossy().into_owned();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistence_requires_password_opt_in_and_preserves_advanced_fields() {
        let dir = tempfile::tempdir().unwrap();
        let mut profile = Profile::default();
        profile.settings.client_password = "private credential".into();
        profile.settings.obfuscation.server_id = "custom-server".into();
        let mut profiles = Profiles {
            selected: 0,
            profiles: vec![profile],
        };
        save(dir.path(), &profiles).unwrap();
        assert!(
            !fs::read_to_string(dir.path().join("profiles.json"))
                .unwrap()
                .contains("private credential")
        );
        assert_eq!(
            load(dir.path()).unwrap().profiles[0]
                .settings
                .obfuscation
                .server_id,
            "custom-server"
        );
        profiles.profiles[0].remember_password = true;
        save(dir.path(), &profiles).unwrap();
        assert_eq!(
            load(dir.path()).unwrap().profiles[0]
                .settings
                .client_password,
            "private credential"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(dir.path().join("profiles.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn corrupt_profiles_are_reported_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("profiles.json"), "broken").unwrap();
        assert!(load(dir.path()).is_err());
        assert_eq!(
            fs::read_to_string(dir.path().join("profiles.json")).unwrap(),
            "broken"
        );
    }

    #[test]
    fn imported_patterns_are_validated_private_and_independent_of_source() {
        let dir = tempfile::tempdir().unwrap();
        prepare(dir.path()).unwrap();
        let source = tempfile::tempdir().unwrap();
        prepare(source.path()).unwrap();
        let client = import_pattern(
            dir.path(),
            &source.path().join("cli_pattern.bin"),
            PatternKind::Client,
        )
        .unwrap();
        let server = import_pattern(
            dir.path(),
            &source.path().join("serv_pattern.bin"),
            PatternKind::Server,
        )
        .unwrap();
        let another = import_pattern(
            dir.path(),
            &source.path().join("cli_pattern.bin"),
            PatternKind::Client,
        )
        .unwrap();
        assert_ne!(client, another);
        assert!(std::path::Path::new(&client).is_relative());
        let original = fs::read(dir.path().join("cli_pattern.bin")).unwrap();
        drop(source);
        let mut profile = Profile::default();
        profile.settings.patterns.client_pattern_path = client.clone();
        profile.settings.patterns.server_pattern_path = server;
        resolve_patterns(&mut profile, dir.path());
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(profile.settings.patterns.load())
            .unwrap();
        let invalid = dir.path().join("invalid.bin");
        fs::write(&invalid, b"not a captured pattern").unwrap();
        assert!(import_pattern(dir.path(), &invalid, PatternKind::Client).is_err());
        assert_eq!(
            fs::read(dir.path().join("cli_pattern.bin")).unwrap(),
            original
        );
        assert_eq!(fs::read(dir.path().join(&client)).unwrap(), original);
        let oversized = fs::File::create(dir.path().join("too-large.bin")).unwrap();
        oversized.set_len(MAX_PATTERN_BYTES + 1).unwrap();
        assert!(
            import_pattern(
                dir.path(),
                &dir.path().join("too-large.bin"),
                PatternKind::Server
            )
            .is_err()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(dir.path().join(client))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn forwarding_validates_destinations_and_listener_ports() {
        let mut profile = Profile::default();
        profile.settings.client_identity = "alice".into();
        profile.settings.client_password = "secret".into();
        profile.mode = Mode::Tcp;
        profile.destination = "[::1]:8090".into();
        assert!(profile.validate().is_ok());
        profile.destination = "missing-port".into();
        assert!(profile.validate().is_err());
        profile.destination = "example.org:443".into();
        profile.settings.local_proxy_bind = "127.0.0.1:0".into();
        assert!(profile.validate().is_err());
    }
}
