use std::path::PathBuf;

pub type ImportResult = Result<Option<PathBuf>, String>;

pub fn init() {
    #[cfg(target_os = "ios")]
    unsafe {
        bluesky_ios_init()
    };
}

#[cfg(any(target_os = "android", target_os = "ios"))]
static IMPORTS: std::sync::Mutex<std::collections::VecDeque<ImportResult>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());

pub fn take_import() -> Option<ImportResult> {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    return IMPORTS.lock().unwrap().pop_front();
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    None
}

pub fn pick_pattern() -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        android::call("choosePattern")?;
        return Ok(true);
    }
    #[cfg(target_os = "ios")]
    {
        unsafe { bluesky_ios_choose_pattern() };
        return Ok(true);
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    Ok(false)
}

pub fn start_connection() -> Result<(), String> {
    #[cfg(target_os = "android")]
    android::start()?;
    #[cfg(target_os = "ios")]
    unsafe {
        bluesky_ios_start()
    };
    Ok(())
}

pub fn stop_connection() {
    #[cfg(target_os = "android")]
    if let Err(error) = android::call("stopTunnel") {
        eprintln!("{error}");
    }
    #[cfg(target_os = "ios")]
    unsafe {
        bluesky_ios_stop()
    };
}

pub fn battery_settings() -> Result<(), String> {
    #[cfg(target_os = "android")]
    return android::call("batterySettings");
    #[cfg(not(target_os = "android"))]
    Ok(())
}

pub fn background_info() -> &'static str {
    #[cfg(target_os = "android")]
    return "While connected, a foreground service and wake lock keep the tunnel running with the screen off. Set battery usage to Unrestricted in Android settings. Force stop still disconnects the tunnel.";
    #[cfg(target_os = "ios")]
    return "iOS allows only a short background grace period for this local proxy. It disconnects when that time expires. Continuous background tunneling requires a VPN Network Extension; it is not available in this build.";
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    "The tunnel stays active while the application is running and reconnects after transport failures."
}

#[cfg(target_os = "android")]
pub use android::init as init_android;

#[cfg(target_os = "android")]
mod android {
    use super::*;
    use jni::{
        JNIEnv, JavaVM,
        objects::{GlobalRef, JClass, JString},
    };
    use std::sync::{Condvar, Mutex};

    static SERVICE_READY: (Mutex<Option<Result<(), String>>>, Condvar) =
        (Mutex::new(None), Condvar::new());

    pub fn start() -> Result<(), String> {
        *SERVICE_READY.0.lock().unwrap() = None;
        call("startTunnel")?;
        // Slint runs on NativeActivity's Rust thread; the Java main thread can
        // acknowledge foreground promotion before any connection is started.
        let (mut result, _) = SERVICE_READY
            .1
            .wait_timeout_while(
                SERVICE_READY.0.lock().unwrap(),
                std::time::Duration::from_secs(5),
                |ready| ready.is_none(),
            )
            .unwrap();
        let result = result
            .take()
            .unwrap_or_else(|| Err("Android foreground service did not start in time.".into()));
        if result.is_err() {
            let _ = call("stopTunnel");
        }
        result
    }

    struct Bridge {
        vm: JavaVM,
        class: GlobalRef,
    }
    static BRIDGE: Mutex<Option<Bridge>> = Mutex::new(None);

    pub fn init(app: &slint::android::AndroidApp) -> Result<(), String> {
        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }.map_err(|e| e.to_string())?;
        let class = {
            let env = vm.attach_current_thread().map_err(|e| e.to_string())?;
            let activity = unsafe { jni::objects::JObject::from_raw(app.activity_as_ptr().cast()) };
            let class = env.get_object_class(&activity).map_err(|e| e.to_string())?;
            env.new_global_ref(class).map_err(|e| e.to_string())?
        };
        *BRIDGE.lock().unwrap() = Some(Bridge { vm, class });
        Ok(())
    }

    pub fn call(method: &str) -> Result<(), String> {
        let bridge = BRIDGE.lock().unwrap();
        let bridge = bridge.as_ref().ok_or("Android bridge is not initialized")?;
        let mut env = bridge
            .vm
            .attach_current_thread()
            .map_err(|e| e.to_string())?;
        let class: &JClass = bridge.class.as_obj().into();
        if let Err(error) = env.call_static_method(class, method, "()V", &[]) {
            // A pending Java exception must not leak into later JNI calls.
            let _ = env.exception_clear();
            return Err(format!("Android {method} failed: {error}"));
        }
        Ok(())
    }

    #[unsafe(no_mangle)]
    extern "system" fn Java_app_bluesky_tunnel_TunnelActivity_patternResult(
        mut env: JNIEnv,
        _class: JClass,
        path: JString,
        error: JString,
    ) {
        let result = (|| {
            let error: String = env.get_string(&error).map_err(|e| e.to_string())?.into();
            if !error.is_empty() {
                return Err(error);
            }
            let path: String = env.get_string(&path).map_err(|e| e.to_string())?.into();
            Ok(if path.is_empty() {
                None
            } else {
                Some(path.into())
            })
        })();
        IMPORTS.lock().unwrap().push_back(result);
    }

    #[unsafe(no_mangle)]
    extern "system" fn Java_app_bluesky_tunnel_TunnelActivity_flushProfiles(
        _env: JNIEnv,
        _class: JClass,
    ) {
        let _ = slint::invoke_from_event_loop(crate::app::flush_pending);
    }

    #[unsafe(no_mangle)]
    extern "system" fn Java_app_bluesky_tunnel_TunnelService_serviceReady(
        mut env: JNIEnv,
        _class: JClass,
        error: JString,
    ) {
        let result = env
            .get_string(&error)
            .map_err(|e| e.to_string())
            .and_then(|error| {
                let error: String = error.into();
                if error.is_empty() { Ok(()) } else { Err(error) }
            });
        *SERVICE_READY.0.lock().unwrap() = Some(result);
        SERVICE_READY.1.notify_one();
    }

    #[unsafe(no_mangle)]
    extern "system" fn Java_app_bluesky_tunnel_TunnelService_stopNative(
        _env: JNIEnv,
        _class: JClass,
    ) {
        if let Ok(engine) = crate::engine::Engine::new() {
            engine.send(crate::engine::Command::Stop(engine.next_generation()));
        }
    }
}

#[cfg(target_os = "ios")]
unsafe extern "C" {
    fn bluesky_ios_init();
    fn bluesky_ios_choose_pattern();
    fn bluesky_ios_start();
    fn bluesky_ios_stop();
}

#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
extern "C" fn bluesky_flush_profiles() {
    crate::app::flush_pending();
}

#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
extern "C" fn bluesky_pattern_result(
    path: *const std::ffi::c_char,
    error: *const std::ffi::c_char,
) {
    let path = unsafe { std::ffi::CStr::from_ptr(path) }.to_string_lossy();
    let error = unsafe { std::ffi::CStr::from_ptr(error) }.to_string_lossy();
    let result = if !error.is_empty() {
        Err(error.into_owned())
    } else {
        Ok(if path.is_empty() {
            None
        } else {
            Some(PathBuf::from(path.as_ref()))
        })
    };
    IMPORTS.lock().unwrap().push_back(result);
}

#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
extern "C" fn bluesky_background_expired() {
    // End the tunnel explicitly rather than leave the UI claiming it is connected
    // while UIKit suspends its process. The user can reconnect on return.
    IOS_EXPIRED.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Ok(engine) = crate::engine::Engine::new() {
        engine.send(crate::engine::Command::Stop(engine.next_generation()));
    }
}

#[cfg(target_os = "ios")]
static IOS_EXPIRED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn background_expired() -> bool {
    #[cfg(target_os = "ios")]
    return IOS_EXPIRED.swap(false, std::sync::atomic::Ordering::Relaxed);
    #[cfg(not(target_os = "ios"))]
    false
}
