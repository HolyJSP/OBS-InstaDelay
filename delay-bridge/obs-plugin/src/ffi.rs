//! The OBS C functions this plugin uses, looked up at runtime in the already-loaded OBS
//! libraries. That way the plugin builds with plain `cargo build` — no OBS SDK required.
//!
//! Every field's type must match the C signature exactly. Missing functions (older OBS) are
//! `None` and the feature that needs them is skipped.

use std::ffi::{CStr, c_char, c_void};

pub type HotkeyFn =
    unsafe extern "C" fn(data: *mut c_void, id: usize, hotkey: *mut c_void, pressed: bool);
pub type MenuFn = unsafe extern "C" fn(data: *mut c_void);
pub type SaveFn = unsafe extern "C" fn(save_data: *mut c_void, saving: bool, data: *mut c_void);
pub type TaskFn = unsafe extern "C" fn(param: *mut c_void);

pub type P = *mut c_void;
pub type CP = *const c_void;
pub type S = *const c_char;

#[derive(Default)]
pub struct Api {
    // logging / memory / modules
    pub blog: Option<unsafe extern "C" fn(i32, S, ...)>,
    pub bfree: Option<unsafe extern "C" fn(P)>,
    pub get_module_binary_path: Option<unsafe extern "C" fn(CP) -> S>,
    pub module_get_config_path: Option<unsafe extern "C" fn(P, S) -> *mut c_char>,
    pub get_version_string: Option<unsafe extern "C" fn() -> S>,
    pub queue_task: Option<unsafe extern "C" fn(i32, TaskFn, P, bool)>,
    // hotkeys
    pub hotkey_register_frontend: Option<unsafe extern "C" fn(S, S, HotkeyFn, P) -> usize>,
    pub hotkey_unregister: Option<unsafe extern "C" fn(usize)>,
    pub hotkey_save: Option<unsafe extern "C" fn(usize) -> P>,
    pub hotkey_load: Option<unsafe extern "C" fn(usize, P)>,
    // obs_data
    pub data_create: Option<unsafe extern "C" fn() -> P>,
    pub data_create_from_json: Option<unsafe extern "C" fn(S) -> P>,
    pub data_release: Option<unsafe extern "C" fn(P)>,
    pub data_get_json: Option<unsafe extern "C" fn(P) -> S>,
    pub data_get_string: Option<unsafe extern "C" fn(P, S) -> S>,
    pub data_get_int: Option<unsafe extern "C" fn(P, S) -> i64>,
    pub data_set_string: Option<unsafe extern "C" fn(P, S, S)>,
    pub data_set_bool: Option<unsafe extern "C" fn(P, S, bool)>,
    pub data_set_array: Option<unsafe extern "C" fn(P, S, P)>,
    pub data_get_array: Option<unsafe extern "C" fn(P, S) -> P>,
    pub data_array_release: Option<unsafe extern "C" fn(P)>,
    // services
    pub service_get_type: Option<unsafe extern "C" fn(CP) -> S>,
    pub service_get_settings: Option<unsafe extern "C" fn(CP) -> P>,
    pub service_get_connect_info: Option<unsafe extern "C" fn(CP, u32) -> S>,
    pub service_create: Option<unsafe extern "C" fn(S, S, P, P) -> P>,
    pub service_release: Option<unsafe extern "C" fn(P)>,
    // outputs / encoders
    pub output_release: Option<unsafe extern "C" fn(P)>,
    pub output_get_total_frames: Option<unsafe extern "C" fn(CP) -> i32>,
    pub output_get_frames_dropped: Option<unsafe extern "C" fn(CP) -> i32>,
    pub output_get_congestion: Option<unsafe extern "C" fn(P) -> f32>,
    pub output_get_total_bytes: Option<unsafe extern "C" fn(CP) -> u64>,
    pub output_get_video_encoder: Option<unsafe extern "C" fn(CP) -> P>,
    pub encoder_get_settings: Option<unsafe extern "C" fn(CP) -> P>,
    pub encoder_get_id: Option<unsafe extern "C" fn(CP) -> S>,
    // config files
    pub config_get_string: Option<unsafe extern "C" fn(P, S, S) -> S>,
    pub config_set_string: Option<unsafe extern "C" fn(P, S, S, S)>,
    pub config_save_safe: Option<unsafe extern "C" fn(P, S, S) -> i32>,
    // frontend
    pub add_tools_menu_item: Option<unsafe extern "C" fn(S, MenuFn, P)>,
    pub add_save_callback: Option<unsafe extern "C" fn(SaveFn, P)>,
    pub remove_save_callback: Option<unsafe extern "C" fn(SaveFn, P)>,
    pub get_streaming_service: Option<unsafe extern "C" fn() -> P>,
    pub set_streaming_service: Option<unsafe extern "C" fn(P)>,
    pub save_streaming_service: Option<unsafe extern "C" fn()>,
    pub streaming_active: Option<unsafe extern "C" fn() -> bool>,
    pub get_streaming_output: Option<unsafe extern "C" fn() -> P>,
    pub get_user_config: Option<unsafe extern "C" fn() -> P>,
    pub get_global_config: Option<unsafe extern "C" fn() -> P>,
}

const LIBOBS: &str = if cfg!(windows) { "obs.dll" } else { "libobs" };
const FRONTEND: &str = if cfg!(windows) {
    "obs-frontend-api.dll"
} else {
    "obs-frontend-api"
};

macro_rules! sym {
    ($lib:expr, $name:literal) => {{
        let p = sys::sym($lib, $name);
        // SAFETY: the symbol is the OBS C function with the signature declared in `Api`.
        if p.is_null() {
            None
        } else {
            Some(unsafe { std::mem::transmute::<*mut c_void, _>(p) })
        }
    }};
}

impl Api {
    // The target type of each transmute is the declared type of the struct field it's assigned to.
    #[allow(clippy::missing_transmute_annotations)]
    pub fn load() -> Api {
        Api {
            blog: sym!(LIBOBS, c"blog"),
            bfree: sym!(LIBOBS, c"bfree"),
            get_module_binary_path: sym!(LIBOBS, c"obs_get_module_binary_path"),
            module_get_config_path: sym!(LIBOBS, c"obs_module_get_config_path"),
            get_version_string: sym!(LIBOBS, c"obs_get_version_string"),
            queue_task: sym!(LIBOBS, c"obs_queue_task"),
            hotkey_register_frontend: sym!(LIBOBS, c"obs_hotkey_register_frontend"),
            hotkey_unregister: sym!(LIBOBS, c"obs_hotkey_unregister"),
            hotkey_save: sym!(LIBOBS, c"obs_hotkey_save"),
            hotkey_load: sym!(LIBOBS, c"obs_hotkey_load"),
            data_create: sym!(LIBOBS, c"obs_data_create"),
            data_create_from_json: sym!(LIBOBS, c"obs_data_create_from_json"),
            data_release: sym!(LIBOBS, c"obs_data_release"),
            data_get_json: sym!(LIBOBS, c"obs_data_get_json"),
            data_get_string: sym!(LIBOBS, c"obs_data_get_string"),
            data_get_int: sym!(LIBOBS, c"obs_data_get_int"),
            data_set_string: sym!(LIBOBS, c"obs_data_set_string"),
            data_set_bool: sym!(LIBOBS, c"obs_data_set_bool"),
            data_set_array: sym!(LIBOBS, c"obs_data_set_array"),
            data_get_array: sym!(LIBOBS, c"obs_data_get_array"),
            data_array_release: sym!(LIBOBS, c"obs_data_array_release"),
            service_get_type: sym!(LIBOBS, c"obs_service_get_type"),
            service_get_settings: sym!(LIBOBS, c"obs_service_get_settings"),
            service_get_connect_info: sym!(LIBOBS, c"obs_service_get_connect_info"),
            service_create: sym!(LIBOBS, c"obs_service_create"),
            service_release: sym!(LIBOBS, c"obs_service_release"),
            output_release: sym!(LIBOBS, c"obs_output_release"),
            output_get_total_frames: sym!(LIBOBS, c"obs_output_get_total_frames"),
            output_get_frames_dropped: sym!(LIBOBS, c"obs_output_get_frames_dropped"),
            output_get_congestion: sym!(LIBOBS, c"obs_output_get_congestion"),
            output_get_total_bytes: sym!(LIBOBS, c"obs_output_get_total_bytes"),
            output_get_video_encoder: sym!(LIBOBS, c"obs_output_get_video_encoder"),
            encoder_get_settings: sym!(LIBOBS, c"obs_encoder_get_settings"),
            encoder_get_id: sym!(LIBOBS, c"obs_encoder_get_id"),
            config_get_string: sym!(LIBOBS, c"config_get_string"),
            config_set_string: sym!(LIBOBS, c"config_set_string"),
            config_save_safe: sym!(LIBOBS, c"config_save_safe"),
            add_tools_menu_item: sym!(FRONTEND, c"obs_frontend_add_tools_menu_item"),
            add_save_callback: sym!(FRONTEND, c"obs_frontend_add_save_callback"),
            remove_save_callback: sym!(FRONTEND, c"obs_frontend_remove_save_callback"),
            get_streaming_service: sym!(FRONTEND, c"obs_frontend_get_streaming_service"),
            set_streaming_service: sym!(FRONTEND, c"obs_frontend_set_streaming_service"),
            save_streaming_service: sym!(FRONTEND, c"obs_frontend_save_streaming_service"),
            streaming_active: sym!(FRONTEND, c"obs_frontend_streaming_active"),
            get_streaming_output: sym!(FRONTEND, c"obs_frontend_get_streaming_output"),
            get_user_config: sym!(FRONTEND, c"obs_frontend_get_user_config"),
            get_global_config: sym!(FRONTEND, c"obs_frontend_get_global_config"),
        }
    }
}

/// Copy a C string owned by OBS (may be NULL).
pub fn cstr(p: S) -> String {
    if p.is_null() {
        String::new()
    } else {
        // SAFETY: OBS returns NUL-terminated strings.
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

#[cfg(windows)]
pub mod sys {
    use std::ffi::{CStr, c_void};

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const i8) -> *mut c_void;
    }
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            hwnd: *mut c_void,
            op: *const u16,
            file: *const u16,
            params: *const u16,
            dir: *const u16,
            show: i32,
        ) -> *mut c_void;
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn sym(lib: &str, name: &CStr) -> *mut c_void {
        let lib = wide(lib);
        // SAFETY: plain Win32 calls with valid NUL-terminated strings.
        unsafe {
            let h = GetModuleHandleW(lib.as_ptr());
            if h.is_null() {
                std::ptr::null_mut()
            } else {
                GetProcAddress(h, name.as_ptr())
            }
        }
    }

    pub fn open_url(url: &str) {
        let (op, url) = (wide("open"), wide(url));
        // SAFETY: valid NUL-terminated wide strings; SW_SHOWNORMAL = 1.
        unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                op.as_ptr(),
                url.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
            );
        }
    }
}

#[cfg(unix)]
pub mod sys {
    use std::ffi::{CStr, c_char, c_void};

    #[cfg_attr(target_os = "linux", link(name = "dl"))]
    unsafe extern "C" {
        fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    }
    #[cfg(target_os = "macos")]
    const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;
    #[cfg(not(target_os = "macos"))]
    const RTLD_DEFAULT: *mut c_void = std::ptr::null_mut();

    pub fn sym(_lib: &str, name: &CStr) -> *mut c_void {
        // SAFETY: dlsym with a valid NUL-terminated name; OBS's libraries are already loaded.
        unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) }
    }

    pub fn open_url(url: &str) {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let _ = std::process::Command::new(opener).arg(url).spawn();
    }
}
