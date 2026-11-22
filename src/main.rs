use delay_bridge::app::App;
use delay_bridge::config::Config;
use delay_bridge::plugin_link::PluginLink;
use delay_bridge::rtmp::server::{self, IngestCtx};
use delay_bridge::web;
use std::path::PathBuf;
use tokio::net::TcpListener;

const HELP: &str = "Delay Bridge — add, change or remove stream delay while live.

USAGE:
    delay-bridge [CONFIG_PATH]

    CONFIG_PATH defaults to config.json next to the program. It is created on first run.

Then point OBS at rtmp://127.0.0.1:1935/live and open http://127.0.0.1:8080 to add your stream key.
Docs: https://github.com/HolyJSP/delay-bridge";

/// True when started by the OBS plugin (runs hidden, no console to read from).
fn launched_by_obs() -> bool {
    std::env::var("DELAY_BRIDGE_LAUNCHED_BY")
        .map(|v| v == "obs")
        .unwrap_or(false)
}

/// Open a URL in the default browser without flashing a console window.
fn open_browser(url: &str) {
    use std::process::{Command, Stdio};
    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        let mut c = Command::new("rundll32.exe");
        c.args(["url.dll,FileProtocolHandler", url])
            .creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let mut cmd = {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    let _ = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn fatal(msg: &str) -> ! {
    eprintln!("\nERROR: {msg}\n");
    // Keep the window open when started by double-click on Windows.
    if cfg!(windows) && !launched_by_obs() {
        eprintln!("Press Enter to close.");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    std::process::exit(1);
}

fn config_path() -> PathBuf {
    if let Some(arg) = std::env::args().nth(1) {
        return PathBuf::from(arg);
    }
    if let Ok(p) = std::env::var("DELAY_BRIDGE_CONFIG") {
        return PathBuf::from(p);
    }
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("config.json")))
        .unwrap_or_else(|| PathBuf::from("config.json"))
}

// Single-threaded runtime: everything here is I/O-bound, and one thread keeps CPU and memory minimal.
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    if arg == "-h" || arg == "--help" {
        println!("{HELP}");
        return;
    }
    if arg == "-V" || arg == "--version" {
        println!("delay-bridge {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let path = config_path();
    let mut cfg = Config::load(&path).unwrap_or_else(|e| fatal(&e));
    cfg.sanitize();

    let ingest = TcpListener::bind((cfg.ingest.host.as_str(), cfg.ingest.port)).await.unwrap_or_else(|e| {
        fatal(&format!(
            "Could not listen for OBS on {}:{} ({e}).\nIs Delay Bridge (or another RTMP server) already running? You can change \"ingest.port\" in {}.",
            cfg.ingest.host, cfg.ingest.port, path.display()
        ))
    });
    let web_listener = TcpListener::bind((cfg.web.host.as_str(), cfg.web.port))
        .await
        .unwrap_or_else(|e| {
            fatal(&format!(
                "Could not start the dashboard on {}:{} ({e}).\nChange \"web.port\" in {}.",
                cfg.web.host,
                cfg.web.port,
                path.display()
            ))
        });

    // When the OBS plugin starts us, stdin/stdout are a private channel to it (stdout must carry
    // nothing else), and stdin closing means OBS has gone away.
    let (obs_gone_tx, obs_gone_rx) = tokio::sync::oneshot::channel::<()>();
    let plugin = launched_by_obs().then(|| PluginLink::start(obs_gone_tx));

    let app = App::start(cfg.clone(), path.clone(), plugin.clone());
    let ctx = IngestCtx {
        stream_key: app.ingest_key.clone(),
        active: app.ingest_active.clone(),
        tx: app.engine_tx.clone(),
        log: app.logger(),
    };
    tokio::spawn(server::serve(ingest, ctx));
    tokio::spawn(web::serve(
        web_listener,
        app.clone(),
        cfg.web.host.clone(),
        cfg.web.port,
    ));

    let host = if cfg.ingest.host == "0.0.0.0" {
        "127.0.0.1"
    } else {
        &cfg.ingest.host
    };
    let whost = if cfg.web.host == "0.0.0.0" {
        "127.0.0.1"
    } else {
        &cfg.web.host
    };
    if plugin.is_none() {
        println!();
        println!("  Delay Bridge {} is running", env!("CARGO_PKG_VERSION"));
        println!(
            "  OBS server:  rtmp://{host}:{}/live   (OBS: Settings > Stream > Service: Custom)",
            cfg.ingest.port
        );
        println!(
            "  Dashboard:   http://{whost}:{}        (open once to add your stream key)",
            cfg.web.port
        );
        println!("  Config file: {}", path.display());
        println!("  Press Ctrl+C to quit.");
        println!();
    }

    // Open the dashboard: always when run on its own (unless turned off in config), but when the
    // OBS plugin starts us, only for first-time setup — not every time OBS opens.
    let needs_setup = !cfg
        .destinations
        .iter()
        .any(|d| d.enabled && !d.key.is_empty());
    let has_dock = std::env::var_os("DELAY_BRIDGE_DOCK").is_some();
    let open = if launched_by_obs() {
        needs_setup && !has_dock // the dock inside OBS shows the dashboard already
    } else {
        cfg.web.open_on_start
    };
    if open {
        open_browser(&format!("http://{whost}:{}/", cfg.web.port));
    }

    // Quit on Ctrl+C, or (when started by the OBS plugin) as soon as OBS goes away.
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = async { if plugin.is_some() { let _ = obs_gone_rx.await; } else { std::future::pending::<()>().await } } => {}
    }
    app.log("Shutting down…");
    app.shutdown().await;
}
