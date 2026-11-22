//! OBS Studio plugin for Delay Bridge.
//!
//! * Starts `delay-bridge(.exe)` (shipped next to this plugin) hidden when OBS starts, and stops
//!   it when OBS closes — or crashes. Settings live in OBS's plugin config folder.
//! * Adds a **Delay Bridge** dock (the dashboard, inside OBS).
//! * Lets Delay Bridge read OBS's stream settings and live stats, and switch OBS between streaming
//!   directly and streaming through Delay Bridge (one-click setup).
//! * Adds native OBS hotkeys (Settings → Hotkeys) and **Tools → Delay Bridge**.
//!
//! Builds with plain `cargo build`: OBS functions are looked up at runtime (see `ffi.rs`).

mod dock;
mod ffi;
mod link;
mod obs_info;

use ffi::{Api, sys};
use std::ffi::{CStr, CString, c_char, c_void};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicPtr, AtomicU16, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

static MODULE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static API: OnceLock<Api> = OnceLock::new();
static PORT: AtomicU16 = AtomicU16::new(8080);
static STATE: Mutex<Option<State>> = Mutex::new(None);

const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);
const OBS_TASK_UI: i32 = 0;

struct State {
    child: Option<Child>,
    stdin: link::SharedStdin,
    hotkeys: Vec<usize>,
}

struct Action {
    id: &'static CStr,
    label: &'static CStr,
    path: &'static str,
}

const ACTIONS: [Action; 4] = [
    Action {
        id: c"delay_bridge.toggle",
        label: c"Delay Bridge: Toggle delay",
        path: "/api/toggle",
    },
    Action {
        id: c"delay_bridge.on",
        label: c"Delay Bridge: Turn on delay",
        path: "/api/on",
    },
    Action {
        id: c"delay_bridge.off",
        label: c"Delay Bridge: Remove delay",
        path: "/api/off",
    },
    Action {
        id: c"delay_bridge.extend",
        label: c"Delay Bridge: Add 5 minutes to timer",
        path: "/api/extend?minutes=5",
    },
];

const EXE_NAME: &str = if cfg!(windows) {
    "delay-bridge.exe"
} else {
    "delay-bridge"
};

pub(crate) fn api() -> &'static Api {
    API.get_or_init(Api::load)
}

pub(crate) fn log(msg: &str) {
    if let (Some(blog), Ok(c)) = (api().blog, CString::new(format!("[delay-bridge] {msg}"))) {
        // SAFETY: "%s" with one valid C string argument. 300 = LOG_INFO.
        unsafe { blog(300, c"%s".as_ptr(), c.as_ptr()) };
    }
}

/// Run `f` on OBS's UI thread (needed for changing stream settings) and return its result.
pub(crate) fn on_ui<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> Option<R> {
    type Job = Box<dyn FnOnce() + Send>;
    unsafe extern "C" fn run(p: *mut c_void) {
        // SAFETY: `p` is the Box<Job> leaked below; OBS runs each task exactly once.
        let job = unsafe { Box::from_raw(p as *mut Job) };
        job();
    }
    let queue = api().queue_task?;
    let (tx, rx) = std::sync::mpsc::channel();
    let job: Box<Job> = Box::new(Box::new(move || {
        let _ = tx.send(f());
    }));
    // SAFETY: ownership of the job passes to the task; we don't wait inside OBS (wait = false).
    unsafe { queue(OBS_TASK_UI, run, Box::into_raw(job) as *mut c_void, false) };
    rx.recv_timeout(Duration::from_secs(10)).ok()
}

fn module_config_path(file: &CStr) -> Option<PathBuf> {
    let (get, free) = (api().module_get_config_path?, api().bfree?);
    // SAFETY: OBS returns a bmalloc'd C string (or NULL) that we copy and then bfree.
    unsafe {
        let p = get(MODULE.load(Ordering::SeqCst), file.as_ptr());
        if p.is_null() {
            return None;
        }
        let s = CStr::from_ptr(p).to_string_lossy().into_owned();
        free(p.cast());
        Some(PathBuf::from(s))
    }
}

fn plugin_dir() -> Option<PathBuf> {
    let get = api().get_module_binary_path?;
    // SAFETY: returns a C string owned by OBS for the module's lifetime.
    let p = unsafe { get(MODULE.load(Ordering::SeqCst)) };
    let path = PathBuf::from(ffi::cstr(p));
    path.parent()
        .filter(|_| !p.is_null())
        .map(Path::to_path_buf)
}

fn read_port(config: &Path) -> u16 {
    std::fs::read_to_string(config)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v["web"]["port"].as_u64())
        .map(|p| p as u16)
        .unwrap_or(8080)
}

fn start_bridge(
    exe: &Path,
    config: &Path,
    log_path: Option<&Path>,
    has_dock: bool,
) -> Option<Child> {
    let mut cmd = Command::new(exe);
    // stdin/stdout are a private channel to the plugin (see link.rs). stdin also stays open for
    // OBS's whole lifetime: when OBS exits or crashes the pipe closes and Delay Bridge shuts down.
    cmd.arg(config)
        .env("DELAY_BRIDGE_LAUNCHED_BY", "obs")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(l) = log_path {
        cmd.env("DELAY_BRIDGE_LOG", l);
    }
    if has_dock {
        cmd.env("DELAY_BRIDGE_DOCK", "1"); // dashboard lives in OBS — don't open a browser tab
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    match cmd.spawn() {
        Ok(c) => Some(c),
        Err(e) => {
            log(&format!("could not start {}: {e}", exe.display()));
            None
        }
    }
}

/// Send one API request to Delay Bridge on a background thread (never blocks OBS).
fn call_api(path: &'static str) {
    let port = PORT.load(Ordering::SeqCst);
    std::thread::spawn(move || {
        let result = (|| -> std::io::Result<String> {
            let addr = SocketAddr::from(([127, 0, 0, 1], port));
            let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(1))?;
            s.set_read_timeout(Some(Duration::from_secs(8)))?;
            write!(
                s,
                "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
            )?;
            let mut resp = String::new();
            s.read_to_string(&mut resp)?;
            Ok(resp.lines().next().unwrap_or("").to_string())
        })();
        match result {
            Ok(status) if status.contains(" 200 ") => {}
            Ok(status) => log(&format!("{path} → {status}")),
            Err(e) => log(&format!(
                "Delay Bridge is not reachable on port {port}: {e}"
            )),
        }
    });
}

// ---- callbacks ------------------------------------------------------------------------------

unsafe extern "C" fn on_hotkey(data: *mut c_void, _id: usize, _hotkey: *mut c_void, pressed: bool) {
    if pressed && let Some(a) = ACTIONS.get(data as usize) {
        call_api(a.path);
    }
}

unsafe extern "C" fn on_menu(_data: *mut c_void) {
    sys::open_url(&format!(
        "http://127.0.0.1:{}/",
        PORT.load(Ordering::SeqCst)
    ));
}

/// Hotkey bindings are stored in OBS's save data (like other plugins do).
unsafe extern "C" fn on_save(save_data: *mut c_void, saving: bool, _data: *mut c_void) {
    let a = api();
    let (Some(save), Some(load), Some(set), Some(get), Some(release)) = (
        a.hotkey_save,
        a.hotkey_load,
        a.data_set_array,
        a.data_get_array,
        a.data_array_release,
    ) else {
        return;
    };
    let ids = match STATE.lock() {
        Ok(s) => s.as_ref().map(|s| s.hotkeys.clone()).unwrap_or_default(),
        Err(_) => return,
    };
    for (action, id) in ACTIONS.iter().zip(ids) {
        // SAFETY: OBS data objects passed straight back to OBS; arrays are released as required.
        unsafe {
            if saving {
                let arr = save(id);
                if !arr.is_null() {
                    set(save_data, action.id.as_ptr(), arr);
                    release(arr);
                }
            } else {
                let arr = get(save_data, action.id.as_ptr());
                if !arr.is_null() {
                    load(id, arr);
                    release(arr);
                }
            }
        }
    }
}

// ---- module exports -------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn obs_module_set_pointer(module: *mut c_void) {
    MODULE.store(module, Ordering::SeqCst);
}

#[unsafe(no_mangle)]
pub extern "C" fn obs_current_module() -> *mut c_void {
    MODULE.load(Ordering::SeqCst)
}

/// libobs API version this plugin targets (30.0.0); works with OBS 30 and newer.
#[unsafe(no_mangle)]
pub extern "C" fn obs_module_ver() -> u32 {
    30 << 24
}

#[unsafe(no_mangle)]
pub extern "C" fn obs_module_name() -> *const c_char {
    c"Delay Bridge".as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn obs_module_description() -> *const c_char {
    c"Add, change or remove stream delay while live (dock, hotkeys, one-click setup from your OBS stream settings).".as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn obs_module_load() -> bool {
    let a = api();
    let config = module_config_path(c"config.json")
        .unwrap_or_else(|| PathBuf::from("delay-bridge-config.json"));
    if let Some(dir) = config.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let log_path = module_config_path(c"delay-bridge.log");
    let state_path = module_config_path(c"plugin-state.json")
        .unwrap_or_else(|| PathBuf::from("delay-bridge-plugin-state.json"));
    let port = read_port(&config);
    PORT.store(port, Ordering::SeqCst);

    let has_dock = dock::ensure(port, &state_path);

    let stdin: link::SharedStdin = Arc::new(Mutex::new(None));
    let child = match plugin_dir().map(|d| d.join(EXE_NAME)) {
        Some(exe) if exe.exists() => {
            log(&format!(
                "starting {} (settings: {})",
                exe.display(),
                config.display()
            ));
            start_bridge(&exe, &config, log_path.as_deref(), has_dock).map(|mut c| {
                *stdin.lock().unwrap_or_else(|e| e.into_inner()) = c.stdin.take();
                if let Some(out) = c.stdout.take() {
                    link::start(out, stdin.clone());
                }
                c
            })
        }
        other => {
            log(&format!(
                "{EXE_NAME} not found next to the plugin ({other:?}); start Delay Bridge yourself — hotkeys still work"
            ));
            None
        }
    };

    let mut hotkeys = Vec::new();
    if let Some(register) = a.hotkey_register_frontend {
        for (i, action) in ACTIONS.iter().enumerate() {
            // SAFETY: OBS copies the name/description strings; data is just the action index.
            hotkeys.push(unsafe {
                register(
                    action.id.as_ptr(),
                    action.label.as_ptr(),
                    on_hotkey,
                    i as *mut c_void,
                )
            });
        }
    }
    if let Ok(mut s) = STATE.lock() {
        *s = Some(State {
            child,
            stdin,
            hotkeys,
        });
    }
    // SAFETY: registering plain C callbacks with OBS's frontend API.
    unsafe {
        if let Some(add) = a.add_save_callback {
            add(on_save, std::ptr::null_mut());
        }
        if let Some(add) = a.add_tools_menu_item {
            add(c"Delay Bridge".as_ptr(), on_menu, std::ptr::null_mut());
        }
    }
    log(&format!("loaded — dashboard at http://127.0.0.1:{port}/"));
    true
}

#[unsafe(no_mangle)]
pub extern "C" fn obs_module_unload() {
    let a = api();
    // SAFETY: removing the callback we added in obs_module_load.
    unsafe {
        if let Some(remove) = a.remove_save_callback {
            remove(on_save, std::ptr::null_mut());
        }
    }
    let state = STATE.lock().ok().and_then(|mut s| s.take());
    if let Some(mut st) = state {
        if let Some(unregister) = a.hotkey_unregister {
            for id in st.hotkeys.drain(..) {
                // SAFETY: ids returned by obs_hotkey_register_frontend.
                unsafe { unregister(id) };
            }
        }
        // Close Delay Bridge's stdin so it shuts down gracefully (closing platform connections
        // cleanly), then make sure it's gone.
        drop(st.stdin.lock().ok().and_then(|mut s| s.take()));
        if let Some(mut child) = st.child.take() {
            let deadline = std::time::Instant::now() + SHUTDOWN_GRACE;
            while std::time::Instant::now() < deadline {
                if let Ok(Some(_)) = child.try_wait() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            if let Ok(None) = child.try_wait() {
                let _ = child.kill();
                let _ = child.wait();
            }
            log("stopped Delay Bridge");
        }
    }
}
