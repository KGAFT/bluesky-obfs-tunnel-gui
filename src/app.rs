use crate::{
    AppWindow,
    engine::{Command, Engine, Phase},
    platform,
    profiles::{self, Mode, PatternKind, Profile, Profiles},
};
use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::{
    cell::RefCell,
    collections::VecDeque,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

struct State {
    dir: PathBuf,
    profiles: Profiles,
    engine: Engine,
    generation: u64,
    log: VecDeque<String>,
    launched: Instant,
    connected_at: Option<Instant>,
    saved: Vec<u8>,
    pending_save: Option<(Vec<u8>, Instant)>,
    pending_import: Option<PatternKind>,
}

thread_local! {
    static FLUSH: RefCell<Option<Box<dyn Fn()>>> = RefCell::new(None);
}

#[cfg(any(target_os = "android", target_os = "ios"))]
pub(crate) fn flush_pending() {
    FLUSH.with(|flush| {
        if let Some(flush) = flush.borrow().as_ref() {
            flush();
        }
    });
}

fn from_form(ui: &AppWindow, base: &Profile) -> Profile {
    let mut profile = base.clone();
    profile.name = ui.get_profile_name().trim().into();
    profile.mode = Mode::from_index(ui.get_mode());
    profile.destination = ui.get_destination().trim().into();
    profile.remember_password = ui.get_remember_password();
    profile.settings.server_address = ui.get_server().trim().into();
    profile.settings.local_proxy_bind = ui.get_listener().trim().into();
    profile.settings.client_identity = ui.get_identity().trim().into();
    profile.settings.client_password = ui.get_password().into();
    if ui.get_multipath() {
        profile
            .settings
            .multipath
            .get_or_insert_with(Default::default);
    } else {
        profile.settings.multipath = None;
    }
    profile.settings.reconnect.enabled = ui.get_rotate();
    profile.settings.patterns.client_pattern_path = ui.get_client_pattern().trim().into();
    profile.settings.patterns.server_pattern_path = ui.get_server_pattern().trim().into();
    profile
}

fn show_profile(ui: &AppWindow, profiles: &Profiles) {
    let profile = &profiles.profiles[profiles.selected];
    ui.set_profile_names(ModelRc::new(VecModel::from(
        profiles
            .profiles
            .iter()
            .map(|p| SharedString::from(&p.name))
            .collect::<Vec<_>>(),
    )));
    ui.set_selected_profile(profiles.selected as i32);
    ui.set_profile_name(profile.name.as_str().into());
    ui.set_mode(profile.mode.index());
    ui.set_destination(profile.destination.as_str().into());
    ui.set_remember_password(profile.remember_password);
    ui.set_server(profile.settings.server_address.as_str().into());
    ui.set_listener(profile.settings.local_proxy_bind.as_str().into());
    ui.set_identity(profile.settings.client_identity.as_str().into());
    ui.set_password(profile.settings.client_password.as_str().into());
    ui.set_multipath(profile.settings.multipath.is_some());
    ui.set_rotate(profile.settings.reconnect.enabled);
    ui.set_client_pattern(
        profile
            .settings
            .patterns
            .client_pattern_path
            .as_str()
            .into(),
    );
    ui.set_server_pattern(
        profile
            .settings
            .patterns
            .server_pattern_path
            .as_str()
            .into(),
    );
    let json = serde_json::to_string_pretty(&profile.settings).unwrap_or_default();
    ui.set_config_baseline(json.as_str().into());
    ui.set_config_json(json.into());
}

fn capture(ui: &AppWindow, state: &mut State) {
    let selected = state.profiles.selected;
    state.profiles.profiles[selected] = from_form(ui, &state.profiles.profiles[selected]);
    profiles::cache_session(&state.dir, &state.profiles);
}

fn persist(state: &mut State) -> Result<(), String> {
    profiles::save(&state.dir, &state.profiles)
        .map_err(|error| format!("Could not save profile: {error}"))?;
    state.saved = serde_json::to_vec(&state.profiles).unwrap();
    state.pending_save = None;
    Ok(())
}

fn finish_import(ui: &AppWindow, state: &mut State, kind: PatternKind, source: PathBuf) {
    let result = profiles::import_pattern(&state.dir, &source, kind)
        .map_err(|e| e.to_string())
        .and_then(|path| {
            let selected = state.profiles.selected;
            let previous = kind
                .path_mut(&mut state.profiles.profiles[selected])
                .clone();
            *kind.path_mut(&mut state.profiles.profiles[selected]) = path;
            if let Err(error) = persist(state) {
                *kind.path_mut(&mut state.profiles.profiles[selected]) = previous;
                return Err(error);
            }
            Ok(())
        });
    show_profile(ui, &state.profiles);
    ui.set_notice(match result {
        Ok(()) => {
            "Pattern imported and saved. Import the matching file for the other direction too."
                .into()
        }
        Err(error) => format!("Pattern import failed: {error}").into(),
    });
}

fn log_event(ui: &AppWindow, state: &mut State, message: &str) {
    let seconds = state.launched.elapsed().as_secs();
    state.log.push_back(format!(
        "{:02}:{:02}:{:02}  {message}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    ));
    while state.log.len() > 100 {
        state.log.pop_front();
    }
    ui.set_activity(
        state
            .log
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n\n")
            .into(),
    );
}

fn bind(ui: &AppWindow, state: Rc<RefCell<State>>) -> Timer {
    let weak = ui.as_weak();
    let shared = state.clone();
    ui.on_toggle_connection(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let mut state = shared.borrow_mut();
        if ui.get_stopping() {
            return;
        }
        state.generation = state.engine.next_generation();
        if ui.get_running() {
            ui.set_stopping(true);
            ui.set_connected(false);
            ui.set_phase("Stopping…".into());
            state.engine.send(Command::Stop(state.generation));
            return;
        }
        capture(&ui, &mut state);
        if let Err(error) = persist(&mut state) {
            ui.set_notice(error.into());
            return;
        }
        let mut profile = state.profiles.profiles[state.profiles.selected].clone();
        if let Err(error) = profile.validate() {
            ui.set_notice(error.into());
            ui.set_page(1);
            return;
        }
        profiles::resolve_patterns(&mut profile, &state.dir);
        if let Err(error) = platform::start_connection() {
            ui.set_notice(error.into());
            return;
        }
        ui.set_notice("".into());
        ui.set_running(true);
        ui.set_phase("Connecting…".into());
        state
            .engine
            .send(Command::Start(state.generation, Box::new(profile)));
    });

    let weak = ui.as_weak();
    let shared = state.clone();
    ui.on_save_profile(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if ui.get_running() {
            return;
        }
        let mut state = shared.borrow_mut();
        capture(&ui, &mut state);
        let result = persist(&mut state);
        match result {
            Ok(()) => {
                show_profile(&ui, &state.profiles);
                ui.set_notice("Profile saved on this device.".into());
            }
            Err(error) => ui.set_notice(error.into()),
        }
    });

    let weak = ui.as_weak();
    let shared = state.clone();
    ui.on_select_profile(move |index| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if ui.get_running() {
            return;
        }
        let mut state = shared.borrow_mut();
        if index < 0 || index as usize >= state.profiles.profiles.len() {
            return;
        }
        capture(&ui, &mut state);
        let previous = state.profiles.selected;
        state.profiles.selected = index as usize;
        if let Err(error) = persist(&mut state) {
            state.profiles.selected = previous;
            ui.set_selected_profile(previous as i32);
            ui.set_notice(error.into());
            return;
        }
        show_profile(&ui, &state.profiles);
        ui.set_notice("".into());
    });

    let weak = ui.as_weak();
    let shared = state.clone();
    ui.on_add_profile(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if ui.get_running() {
            return;
        }
        let mut state = shared.borrow_mut();
        capture(&ui, &mut state);
        let profile = Profile {
            name: format!("Tunnel {}", state.profiles.profiles.len() + 1),
            ..Profile::default()
        };
        state.profiles.profiles.push(profile);
        state.profiles.selected = state.profiles.profiles.len() - 1;
        show_profile(&ui, &state.profiles);
        ui.set_notice(match persist(&mut state) {
            Ok(()) => "New profile. Changes are saved automatically.".into(),
            Err(error) => error.into(),
        });
    });

    let weak = ui.as_weak();
    let shared = state.clone();
    ui.on_delete_profile(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if ui.get_running() {
            return;
        }
        let mut state = shared.borrow_mut();
        if state.profiles.profiles.len() <= 1 {
            return;
        }
        let mut updated = state.profiles.clone();
        updated.profiles.remove(updated.selected);
        updated.selected = updated.selected.min(updated.profiles.len() - 1);
        match profiles::save(&state.dir, &updated) {
            Ok(()) => {
                state.profiles = updated;
                state.saved = serde_json::to_vec(&state.profiles).unwrap();
                state.pending_save = None;
                show_profile(&ui, &state.profiles);
                ui.set_notice("Profile deleted.".into());
            }
            Err(error) => ui.set_notice(error.to_string().into()),
        }
    });

    let weak = ui.as_weak();
    let shared = state.clone();
    ui.on_apply_json(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if ui.get_running() {
            return;
        }
        // Missing credential fields mean keep the form values, never library demo defaults.
        let result = serde_json::from_str::<serde_json::Value>(&ui.get_config_json())
            .map_err(|e| e.to_string())
            .and_then(|mut value| {
                if let Some(object) = value.as_object_mut() {
                    let baseline =
                        serde_json::from_str::<serde_json::Value>(&ui.get_config_baseline())
                            .unwrap_or_default();
                    for (key, current) in [
                        ("client_identity", ui.get_identity()),
                        ("client_password", ui.get_password()),
                    ] {
                        // An unchanged credential in an older editor snapshot must
                        // not roll back a password subsequently typed in the form.
                        if !object.contains_key(key) || object.get(key) == baseline.get(key) {
                            object.insert(key.into(), current.to_string().into());
                        }
                    }
                }
                serde_json::from_value::<bluesky_obfs_tunnel::config::ClientSettings>(value)
                    .map_err(|e| e.to_string())
            })
            .and_then(|settings| {
                settings.validate().map_err(|errors| errors.join("; "))?;
                Ok(settings)
            });
        match result {
            Ok(settings) => {
                let mut state = shared.borrow_mut();
                capture(&ui, &mut state);
                let selected = state.profiles.selected;
                state.profiles.profiles[selected].settings = settings;
                show_profile(&ui, &state.profiles);
                ui.set_notice(match persist(&mut state) {
                    Ok(()) => "JSON applied and saved.".into(),
                    Err(error) => error.into(),
                });
            }
            Err(error) => ui.set_notice(format!("Invalid configuration: {error}").into()),
        }
    });

    let weak = ui.as_weak();
    let shared = state.clone();
    ui.on_refresh_json(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if ui.get_running() {
            return;
        }
        let mut state = shared.borrow_mut();
        capture(&ui, &mut state);
        show_profile(&ui, &state.profiles);
        ui.set_notice("JSON refreshed from the current form.".into());
    });

    let weak = ui.as_weak();
    let shared = state.clone();
    ui.on_clear_activity(move || {
        shared.borrow_mut().log.clear();
        if let Some(ui) = weak.upgrade() {
            ui.set_activity("No events yet.".into());
        }
    });

    let weak = ui.as_weak();
    let shared = state.clone();
    ui.on_remember_changed(move || {
        if let Some(ui) = weak.upgrade() {
            let mut state = shared.borrow_mut();
            capture(&ui, &mut state);
            if let Err(error) = persist(&mut state) {
                ui.set_notice(error.into());
            }
        }
    });

    let weak = ui.as_weak();
    let shared = state.clone();
    ui.on_import_pattern(move |server| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if ui.get_running() || ui.get_importing() {
            return;
        }
        let mut state = shared.borrow_mut();
        capture(&ui, &mut state);
        let kind = if server {
            PatternKind::Server
        } else {
            PatternKind::Client
        };
        match platform::pick_pattern() {
            Ok(true) => {
                state.pending_import = Some(kind);
                ui.set_importing(true);
            }
            Ok(false) => {
                let selected = state.profiles.selected;
                let path = PathBuf::from(&*kind.path_mut(&mut state.profiles.profiles[selected]));
                let path = if path.is_relative() {
                    state.dir.join(path)
                } else {
                    path
                };
                finish_import(&ui, &mut state, kind, path);
            }
            Err(error) => ui.set_notice(error.into()),
        }
    });

    let weak = ui.as_weak();
    ui.on_battery_settings(move || {
        if let Err(error) = platform::battery_settings()
            && let Some(ui) = weak.upgrade()
        {
            ui.set_notice(error.into());
        }
    });

    let timer = Timer::default();
    let weak = ui.as_weak();
    timer.start(TimerMode::Repeated, Duration::from_millis(100), move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let mut state = state.borrow_mut();
        if platform::background_expired() {
            ui.set_notice(
                "iOS background time expired. Reconnect to start the tunnel again.".into(),
            );
        }
        if let Some(result) = platform::take_import()
            && let Some(kind) = state.pending_import.take()
        {
            ui.set_importing(false);
            match result {
                Ok(Some(path)) => {
                    finish_import(&ui, &mut state, kind, path.clone());
                    let _ = std::fs::remove_file(path);
                }
                Ok(None) => (),
                Err(error) => ui.set_notice(error.into()),
            }
        }
        if !ui.get_running() && !ui.get_importing() {
            capture(&ui, &mut state);
            let current = serde_json::to_vec(&state.profiles).unwrap();
            if current == state.saved {
                state.pending_save = None;
            } else if state
                .pending_save
                .as_ref()
                .is_none_or(|(pending, _)| *pending != current)
            {
                state.pending_save = Some((current, Instant::now()));
            } else if state.pending_save.as_ref().unwrap().1.elapsed() >= Duration::from_millis(400)
                && let Err(error) = persist(&mut state)
            {
                ui.set_notice(error.into());
                state.pending_save.as_mut().unwrap().1 = Instant::now() + Duration::from_secs(2);
            }
        }
        while let Ok(event) = state.engine.events.try_recv() {
            if event.generation < state.generation {
                continue;
            }
            state.generation = event.generation;
            let connected = event.phase == Phase::Connected;
            if connected && !ui.get_connected() {
                state.connected_at = Some(Instant::now());
            }
            if !connected {
                state.connected_at = None;
            }
            ui.set_connected(connected);
            ui.set_running(matches!(
                event.phase,
                Phase::Connecting | Phase::Connected | Phase::Retrying
            ));
            ui.set_stopping(false);
            ui.set_phase(
                match event.phase {
                    Phase::Idle => "Ready when you are",
                    Phase::Connecting => "Connecting…",
                    Phase::Connected => "You’re connected",
                    Phase::Retrying => "Reconnecting…",
                    Phase::Failed => "Connection needs attention",
                }
                .into(),
            );
            ui.set_status_message(event.message.as_str().into());
            if event.phase == Phase::Failed {
                ui.set_notice(event.message.as_str().into());
            }
            log_event(&ui, &mut state, &event.message);
        }
        let seconds = state
            .connected_at
            .map_or(0, |start| start.elapsed().as_secs());
        ui.set_uptime(
            format!(
                "{:02}:{:02}:{:02}",
                seconds / 3600,
                seconds / 60 % 60,
                seconds % 60
            )
            .into(),
        );
    });
    timer
}

pub fn run(data_dir: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    platform::init();
    let dir = match data_dir {
        Some(dir) => dir,
        None => profiles::data_dir()?,
    };
    profiles::prepare(&dir)?;
    let profiles = profiles::load_session(&dir)?;
    let ui = AppWindow::new()?;
    ui.set_storage_path(dir.to_string_lossy().as_ref().into());
    ui.set_activity("No events yet. Connect a tunnel to get started.".into());
    ui.set_mobile_import(cfg!(any(target_os = "android", target_os = "ios")));
    ui.set_android(cfg!(target_os = "android"));
    ui.set_background_info(platform::background_info().into());
    show_profile(&ui, &profiles);
    let saved = serde_json::to_vec(&profiles)?;
    let engine = Engine::new()?;
    let snapshot = engine.snapshot();
    ui.set_running(matches!(
        snapshot.phase,
        Phase::Connecting | Phase::Connected | Phase::Retrying
    ));
    ui.set_connected(snapshot.phase == Phase::Connected);
    ui.set_status_message(snapshot.message.into());
    let state = Rc::new(RefCell::new(State {
        dir,
        profiles,
        engine,
        generation: snapshot.generation,
        log: VecDeque::new(),
        launched: Instant::now(),
        connected_at: None,
        saved,
        pending_save: None,
        pending_import: None,
    }));
    let _timer = bind(&ui, state.clone());
    let weak = ui.as_weak();
    let shared = Rc::downgrade(&state);
    FLUSH.with(|flush| {
        *flush.borrow_mut() = Some(Box::new(move || {
            let (Some(ui), Some(shared)) = (weak.upgrade(), shared.upgrade()) else {
                return;
            };
            let mut state = shared.borrow_mut();
            capture(&ui, &mut state);
            if let Err(error) = persist(&mut state) {
                ui.set_notice(error.into());
            }
        }));
    });
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    if std::env::args().any(|arg| arg == "--mobile") {
        ui.window().set_size(slint::LogicalSize::new(390.0, 844.0));
    }
    let weak = ui.as_weak();
    let shared = state.clone();
    ui.window().on_close_requested(move || {
        if let Some(ui) = weak.upgrade() {
            let mut state = shared.borrow_mut();
            capture(&ui, &mut state);
            if let Err(error) = persist(&mut state) {
                ui.set_notice(error.into());
                return slint::CloseRequestResponse::KeepWindowShown;
            }
        }
        slint::CloseRequestResponse::HideWindow
    });
    ui.run()?;
    capture(&ui, &mut state.borrow_mut());
    persist(&mut state.borrow_mut())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_and_json_preserve_advanced_fields_and_reject_invalid_changes() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut profile = Profile::default();
        profile.settings.client_identity = "alice".into();
        profile.settings.client_password = "secret".into();
        profile.settings.obfuscation.server_id = "private-server".into();
        let profiles = Profiles {
            selected: 0,
            profiles: vec![profile],
        };
        show_profile(&ui, &profiles);
        let state = Rc::new(RefCell::new(State {
            dir: dir.path().into(),
            profiles,
            engine: Engine::new().unwrap(),
            generation: 0,
            log: VecDeque::new(),
            launched: Instant::now(),
            connected_at: None,
            saved: Vec::new(),
            pending_save: None,
            pending_import: None,
        }));
        let _timer = bind(&ui, state.clone());
        assert!(!ui.get_multipath());
        ui.set_multipath(true);
        ui.set_server("example.org:443".into());
        ui.invoke_save_profile();
        let loaded = profiles::load(dir.path()).unwrap();
        assert_eq!(
            loaded.profiles[0].settings.multipath,
            Some(Default::default())
        );
        assert_eq!(
            loaded.profiles[0].settings.server_address,
            "example.org:443"
        );
        assert_eq!(
            loaded.profiles[0].settings.obfuscation.server_id,
            "private-server"
        );
        assert!(loaded.profiles[0].settings.client_password.is_empty());
        // Applying edits to an older JSON snapshot must not restore old credentials.
        ui.set_password("replacement-secret".into());
        ui.invoke_apply_json();
        assert_eq!(ui.get_password(), "replacement-secret");
        ui.set_password("secret".into());
        // Save incomplete drafts and remember credentials even before Connect can validate them.
        ui.set_remember_password(true);
        ui.set_server("unfinished-address".into());
        ui.invoke_toggle_connection();
        assert!(!ui.get_running());
        let loaded = profiles::load(dir.path()).unwrap();
        assert_eq!(loaded.profiles[0].settings.client_password, "secret");
        assert_eq!(
            loaded.profiles[0].settings.server_address,
            "unfinished-address"
        );
        ui.set_server("example.org:443".into());
        // JSON with omitted credentials must retain the actual form values.
        ui.set_config_json(
            r#"{"server_address":"example.org:443","multipath":{"max_paths_per_client":2}}"#.into(),
        );
        ui.invoke_apply_json();
        assert!(ui.get_multipath());
        ui.invoke_refresh_json();
        let settings: bluesky_obfs_tunnel::config::ClientSettings =
            serde_json::from_str(&ui.get_config_json()).unwrap();
        assert_eq!(settings.multipath.unwrap().max_paths_per_client, 2);
        assert_eq!(ui.get_identity(), "alice");
        assert_eq!(ui.get_password(), "secret");
        assert_eq!(
            profiles::load(dir.path()).unwrap().profiles[0]
                .settings
                .client_password,
            "secret"
        );
        // Opting out removes the disk copy immediately but retains the session password.
        ui.set_remember_password(false);
        ui.invoke_remember_changed();
        assert!(
            profiles::load(dir.path()).unwrap().profiles[0]
                .settings
                .client_password
                .is_empty()
        );
        assert_eq!(ui.get_password(), "secret");
        // Form edits autosave without requiring a valid completed configuration.
        ui.set_identity("edited-alice".into());
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
        state.borrow_mut().pending_save.as_mut().unwrap().1 =
            Instant::now() - Duration::from_secs(1);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
        assert_eq!(
            profiles::load(dir.path()).unwrap().profiles[0]
                .settings
                .client_identity,
            "edited-alice"
        );
        ui.set_config_json("invalid".into());
        ui.invoke_apply_json();
        assert!(ui.get_notice().starts_with("Invalid configuration"));
        assert_eq!(
            state.borrow().profiles.profiles[0].settings.server_address,
            "example.org:443"
        );
        ui.invoke_add_profile();
        assert_eq!(ui.get_selected_profile(), 1);
        ui.invoke_delete_profile();
        assert_eq!(ui.get_selected_profile(), 0);
        assert!(ui.get_multipath());
        assert_eq!(ui.get_password(), "secret");
        ui.set_multipath(false);
        ui.invoke_save_profile();
        assert!(
            profiles::load(dir.path()).unwrap().profiles[0]
                .settings
                .multipath
                .is_none()
        );
        ui.window().set_size(slint::LogicalSize::new(390.0, 844.0));
        assert!(ui.get_compact());
        ui.set_identity("".into());
        ui.invoke_toggle_connection();
        assert!(!ui.get_running());
        assert_eq!(ui.get_page(), 1);
    }
}
