// SPDX-License-Identifier: GPL-3.0-only
//! `illum`: one binary, every role (frontend, service, backend, control).
//!
//! Runs in a console for now: `illum --service` hosts, `illum view <ip>` opens a viewer window,
//! `illum connect <ip>` measures round-trip time. The Windows service, the backend and the
//! full app window arrive in later milestones.

mod app;
mod control;
mod installed;
mod prompt;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use illum::host::{AudioFactory, HostConfig, SourceFactory, serve_session};
#[cfg(windows)]
use illum::host::{InputFactory, LayoutFn, ObstacleFn};
use illum::viewer::{MediaSlot, Notify, ViewParams, run_viewer_session_sharing_media};
use illum_core::Role;
use illum_core::clipboard::ClipboardMsg;
#[cfg(windows)]
use illum_core::input::InputSink;
use illum_core::protocol::Channel;
use illum_core::protocol::DEFAULT_PORT;
use illum_core::protocol::{ControlMsg, Hello};
use illum_core::store::TrustStore;
use illum_media::MediaError;
use illum_media::audio::AudioSource;
use illum_media::source::{PatternSource, ScreenSource};
use illum_net::session::{accept, connect, normalize_target};
use illum_net::{Endpoint, Identity, Listener, Timeouts, bind};
use illum_ui::{InputOut, KeyboardMode, PointerMode, Shared, UiEvent, ViewerOptions, ViewerWindow};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

#[derive(Debug, Parser)]
#[command(
    name = "illum",
    version,
    about = "Illumination: direct, IP-only remote desktop"
)]
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    /// Folder for this device's key and trust store.
    #[arg(long, global = true, value_name = "DIR")]
    home: Option<PathBuf>,

    /// Show debug logging.
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Run as the host service: listen for incoming connections.
    #[arg(long, group = "role")]
    service: bool,

    /// Per-session worker, started by the service (not implemented yet).
    #[arg(long, group = "role")]
    backend: bool,

    /// Per-user helper for clipboard and tray, started by the service (not implemented yet).
    #[arg(long, group = "role")]
    control: bool,

    /// Port to listen on (with --service).
    #[arg(long, default_value_t = DEFAULT_PORT, requires = "service")]
    port: u16,

    /// Listen on this address instead of the Tailscale addresses (repeatable).
    /// Use it to opt in to a LAN address, or 127.0.0.1 for local testing.
    #[arg(long = "bind", value_name = "IP", requires = "service")]
    bind: Vec<IpAddr>,

    /// Stream the colour test pattern instead of the real screen (with --service).
    #[arg(long, requires = "service")]
    test_pattern: bool,

    /// Never capture or send this machine's sound (with --service).
    #[arg(long, requires = "service")]
    no_audio: bool,

    /// Do not sync the clipboard with the controller (with --service).
    #[arg(long, requires = "service")]
    no_clipboard: bool,

    /// Started by the installed service (`illum service run`), not by hand: asks about new
    /// desks in a window of its own and follows the app's settings (with --service).
    #[arg(long, requires = "service", hide = true)]
    from_service: bool,
    /// Start the app in the tray, its window hidden (what "Start on boot" runs).
    #[arg(long)]
    tray: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Connect to a host by IP or name (port defaults to 7171) and measure round-trip time.
    Connect {
        /// e.g. 100.100.101.1, laptop, or laptop.tail1234.ts.net:7171
        target: String,
        /// How many pings to send before disconnecting.
        #[arg(long, default_value_t = 10)]
        pings: u32,
    },
    /// Open a window showing a host's screen.
    View {
        /// e.g. 100.100.101.1, laptop, or laptop.tail1234.ts.net:7171
        target: String,
        /// Which of the host's displays to show (0 = primary; see `illum displays` on the host).
        #[arg(long, default_value_t = 0)]
        screen: u32,
        /// How keys are sent. remote (the default): key positions, with the keyboard grabbed so
        /// the host's layout and IME apply and no key acts on this machine; local: characters as
        /// typed here, using this machine's IME (for a host without your language's IME); auto:
        /// remote when both layouts match.
        #[arg(long, default_value = "remote", value_parser = parse_keyboard)]
        keyboard: KeyboardMode,
        /// Which pointer you see while you control the host. desk: this desk's own pointers,
        /// following the kind the host shows (arrow, text, hand, ...). host: the host's
        /// pointer pictures. Either way your own system moves it, so it never lags.
        #[arg(long, default_value = "desk", value_parser = parse_pointer)]
        pointer: PointerMode,
        /// Only watch: send no keyboard or mouse input.
        #[arg(long)]
        view_only: bool,
        /// Cap on the host's media rate in Mb/s (the top of the ladder; default 20).
        #[arg(long, default_value_t = 20.0)]
        max_bitrate: f64,
        /// Highest frame rate to ask for (the top of the ladder; default 30, up to 120).
        #[arg(long, default_value_t = 30)]
        max_fps: u32,
        /// Let the host lower the resolution under pressure (off: it lowers bitrate and frame
        /// rate only; scaling costs latency while the host scales on the CPU).
        #[arg(long)]
        allow_scale: bool,
        /// Frames arriving later than this after capture count as late (default 50 ms).
        #[arg(long, default_value_t = 50)]
        latency_limit: u32,
        /// Parity range in percent, for measurement runs only (the loss controller keeps it
        /// between 5 and 30 on its own; it is not a user setting).
        #[arg(long, default_value_t = 5)]
        fec_min: u32,
        #[arg(long, default_value_t = 30)]
        fec_max: u32,
        /// Don't ask for the host's sound.
        #[arg(long)]
        mute: bool,
        /// Don't sync the clipboard with the host (text is synced both ways by default, §8).
        #[arg(long)]
        no_clipboard: bool,
        /// Don't re-dial the host when the connection drops (it reconnects by default).
        #[arg(long)]
        no_reconnect: bool,
        /// Decode on the CPU only (the default uses the GPU's decoder where it exists, and
        /// offers AV1 only then).
        #[arg(long)]
        software_decode: bool,
        /// Draw the window on the CPU (the default converts, scales and presents on the GPU).
        #[arg(long)]
        software_render: bool,
        /// Measurement run: the host streams at exactly --max-bitrate, --max-fps, --fec-min and
        /// --scale, with no adaptation (docs/testing/measure.md).
        #[arg(long)]
        fixed: bool,
        /// Resolution as a percentage of the host display (with --fixed; default native).
        #[arg(long, default_value_t = 100)]
        scale: u32,
        /// Which codec to offer: av1 (AV1 where both sides can, else H.265), hevc, or tiles.
        #[arg(long, value_enum)]
        codec: Option<CodecChoice>,
        /// Close the window after this many seconds.
        #[arg(long, value_name = "SECONDS")]
        duration: Option<u64>,
        /// Write per-second records of the session to this JSON file when it ends.
        #[arg(long, value_name = "FILE")]
        report: Option<PathBuf>,
        /// Name of the step in the report (default: the report file's name).
        #[arg(long)]
        label: Option<String>,
        /// This window is a display popped out of another session window (started by it).
        #[arg(long, hide = true)]
        popout: bool,
        /// The process whose window this one belongs to: it closes when that one is gone.
        #[arg(long, hide = true)]
        parent: Option<u32>,
    },
    /// Show the measurement workload full screen on this machine (the host side of a
    /// measurement run): still, fine detail and full motion, 20 s each, on the wall clock.
    Workload {
        /// Which monitor to fill (0 = primary); the viewer streams it with `view --screen N`.
        #[arg(long, default_value_t = 0)]
        screen: usize,
    },
    /// Turn a folder of `view --report` files into a Markdown table.
    Report {
        /// Folder with the JSON files one run wrote.
        dir: PathBuf,
        /// Where to write the table (default: report.md inside the folder).
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
    },
    /// List this machine's displays.
    Displays,
    /// Show which video codecs this machine can encode and decode (AV1, H.265).
    Codecs,
    /// The installed service: lets this desk be reached at any time, in windows that run as
    /// administrator too. `install` and `uninstall` need an administrator.
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// Ask the person at this desk about a new desk (started by the installed host).
    #[command(hide = true)]
    Ask {
        #[arg(long)]
        name: String,
        #[arg(long)]
        address: String,
        #[arg(long)]
        fingerprint: String,
        #[arg(long, default_value = "")]
        os: String,
    },
    /// Show this device's key fingerprint.
    Fingerprint,
    /// List pinned hosts and paired devices.
    List,
    /// Remove the pinned key for a host address (after the host was reinstalled).
    Forget {
        /// The address exactly as you connected to it, e.g. 100.100.101.1:7171
        address: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
enum ServiceAction {
    /// Register this program as a Windows service, open the firewall for it and start it.
    Install,
    /// Stop and remove the service. Keys, desks and settings are kept.
    Uninstall,
    /// Say whether the service is installed and what its host is doing.
    Status,
    /// What Windows starts (not for use by hand).
    #[command(hide = true)]
    Run,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum CodecChoice {
    Av1,
    Hevc,
    Tiles,
}

impl Cli {
    fn role(&self) -> Role {
        if self.service {
            Role::Service
        } else if self.backend {
            Role::Backend
        } else if self.control {
            Role::Control
        } else {
            Role::Frontend
        }
    }
}

/// Device-wide folder, shared by every role (AnyDesk keeps its key in ProgramData too).
pub(crate) fn default_home() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(|p| PathBuf::from(p).join("Illumination"))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".illumination")))
        .unwrap_or_else(|| PathBuf::from(".illumination"))
}

/// Whether `home` is the folder an installed desk uses, the one the service's host serves.
pub(crate) fn is_usual_home(home: &Path) -> bool {
    home == default_home()
}

/// Where this program keeps files of its own (its log, what it found out about the GPU): the
/// home folder where it may write there, else the person's own folder. On an installed desk
/// the home folder belongs to the system (architecture §3).
pub(crate) fn own_folder(home: &Path) -> PathBuf {
    let probe = home.join(format!(".write-{}", std::process::id()));
    let writable = std::fs::create_dir_all(home).is_ok() && std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    if writable {
        return home.to_path_buf();
    }
    std::env::var_os("LOCALAPPDATA")
        .map(|p| PathBuf::from(p).join("Illumination"))
        .or_else(|| {
            std::env::var_os("XDG_STATE_HOME").map(|p| PathBuf::from(p).join("illumination"))
        })
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state/illumination"))
        })
        .unwrap_or_else(|| home.to_path_buf())
}

fn device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown device".into())
}

fn endpoint(home: &Path) -> Result<Endpoint> {
    let identity = Identity::load_or_create(home).context("loading the device key")?;
    let mut store = TrustStore::load(home.join("trust.json")).context("loading the trust store")?;
    // On an installed desk the store is the service's to write; this program asks it.
    if let Some(pipe) = installed::control_pipe(home) {
        store.write_through(control::trust_through(pipe));
    }
    Ok(Endpoint {
        identity: Arc::new(identity),
        store: Arc::new(Mutex::new(store)),
        device_name: device_name(),
        timeouts: Timeouts::default(),
        prompt: prompt::console_prompt(),
        capabilities: illum_net::session::all_capabilities(),
    })
}

/// Console at info (debug with `-v`), plus one file per role under `<home>/logs` at debug
/// level, so a problem seen once can be read afterwards. Returns the file's path.
fn init_logging(cli: &Cli) -> Option<PathBuf> {
    use tracing_subscriber::Layer;
    use tracing_subscriber::filter::LevelFilter;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let console = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_writer(std::io::stderr)
        // Colour codes only where they are understood: classic Windows consoles print them raw.
        .with_ansi(!cfg!(windows) && std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_filter(if cli.verbose {
            LevelFilter::DEBUG
        } else {
            LevelFilter::INFO
        });
    // The GPU libraries narrate every shader they compile at debug level (megabytes for one
    // window): from them, only what went wrong.
    let quiet_gpu = tracing_subscriber::filter::filter_fn(|meta| {
        let chatty = ["naga", "wgpu"]
            .iter()
            .any(|lib| meta.target().starts_with(lib));
        !chatty || *meta.level() <= tracing::Level::WARN
    });
    let console = console.with_filter(quiet_gpu.clone());
    let role = match (&cli.command, cli.role()) {
        (Some(Command::View { .. }), Role::Frontend) => "viewer".to_string(),
        (None, Role::Frontend) => "app".to_string(),
        (Some(Command::Service { .. }), Role::Frontend) => "service-run".to_string(),
        (Some(Command::Ask { .. }), Role::Frontend) => "ask".to_string(),
        (_, role) => format!("{role:?}").to_lowercase(),
    };
    let path = own_folder(&cli.home.clone().unwrap_or_else(default_home))
        .join("logs")
        .join(format!("{role}.log"));
    let file = std::fs::create_dir_all(path.parent().expect("logs directory"))
        .and_then(|()| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
        })
        .ok();
    let opened = file.is_some();
    let file_layer = file.map(|f| {
        tracing_subscriber::fmt::layer()
            .with_target(true)
            .with_ansi(false)
            .with_writer(Arc::new(f))
            .with_filter(LevelFilter::DEBUG)
            .with_filter(quiet_gpu)
    });
    tracing_subscriber::registry()
        .with(console)
        .with(file_layer)
        .init();
    opened.then_some(path)
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let log_path = init_logging(&cli);
    if let Some(path) = &log_path
        && (cli.role() == Role::Service || matches!(cli.command, Some(Command::View { .. })))
    {
        tracing::info!(target: "illum", "Log file: {} (debug level).", path.display());
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let result = match &cli.command {
        // The window must own the main thread, so the viewer runs outside `block_on`.
        Some(Command::View {
            target,
            screen,
            keyboard,
            pointer,
            view_only,
            max_bitrate,
            max_fps,
            allow_scale,
            latency_limit,
            fec_min,
            fec_max,
            mute,
            no_clipboard,
            no_reconnect,
            software_decode,
            software_render,
            fixed,
            scale,
            codec,
            duration,
            report,
            label,
            popout,
            parent,
        }) if cli.role() == Role::Frontend => {
            let home = cli.home.clone().unwrap_or_else(default_home);
            let options = ViewArgs {
                keyboard: *keyboard,
                pointer: *pointer,
                view_only: *view_only,
                gpu: !*software_render,
                codec: *codec,
                duration: duration.map(Duration::from_secs),
                report: report.clone(),
                label: label.clone(),
                popout: *popout,
                parent: *parent,
                home_flag: cli.home.clone(),
                params: ViewParams {
                    max_bitrate_kbps: (max_bitrate.max(0.1) * 1000.0) as u32,
                    max_fps: *max_fps,
                    fec_min: *fec_min,
                    fec_max: *fec_max,
                    latency_limit_ms: *latency_limit,
                    audio: !*mute,
                    clipboard: !*no_clipboard,
                    reconnect: !*no_reconnect,
                    hw_decode: !*software_decode,
                    fixed: *fixed,
                    scale_percent: *scale,
                    scale_rungs: *allow_scale,
                    record: report.is_some(),
                    ..ViewParams::new(*screen)
                },
            };
            run_view(&runtime, &home, target, options)
        }
        Some(Command::Workload { screen }) if cli.role() == Role::Frontend => {
            illum_ui::run_workload(*screen).map_err(anyhow::Error::msg)
        }
        Some(Command::Ask {
            name,
            address,
            fingerprint,
            os,
        }) if cli.role() == Role::Frontend => {
            return app::run_ask(name, address, fingerprint, os);
        }
        // Windows runs the service on threads of its own choosing.
        Some(Command::Service {
            action: ServiceAction::Run,
        }) if cli.role() == Role::Frontend => installed::run_service(),
        // No command: the app window, which also owns the main thread.
        None if cli.role() == Role::Frontend => {
            let home = cli.home.clone().unwrap_or_else(default_home);
            app::run_app(&runtime, &home, cli.home.clone(), cli.tray)
        }
        _ => runtime.block_on(run(cli)),
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("illum: {err:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    let home = cli.home.clone().unwrap_or_else(default_home);
    let role = cli.role();
    match (role, cli.command) {
        (Role::Service, _) if cli.from_service => {
            installed::run_host(&home, cli.home.clone(), cli.port, &cli.bind).await;
            Ok(())
        }
        (Role::Service, _) => {
            run_service(
                &home,
                cli.port,
                &cli.bind,
                host_config(cli.test_pattern, cli.no_audio, cli.no_clipboard),
            )
            .await
        }
        (Role::Backend | Role::Control, _) => bail!("role {role:?} is not implemented yet"),
        (Role::Frontend, None) => unreachable!("handled in main"),
        (Role::Frontend, Some(Command::Connect { target, pings })) => {
            run_connect(&home, &target, pings).await
        }
        (Role::Frontend, Some(Command::View { .. } | Command::Workload { .. })) => {
            unreachable!("handled in main")
        }
        (Role::Frontend, Some(Command::Report { dir, out })) => {
            let reports = illum::measure::load_dir(&dir)
                .with_context(|| format!("reading {}", dir.display()))?;
            if reports.is_empty() {
                bail!("no report files (*.json) in {}", dir.display());
            }
            let table = illum::measure::markdown(&reports);
            let out = out.unwrap_or_else(|| dir.join("report.md"));
            std::fs::write(&out, &table).with_context(|| format!("writing {}", out.display()))?;
            print!("{table}");
            println!("\nWritten to {}.", out.display());
            Ok(())
        }
        (Role::Frontend, Some(Command::Service { action })) => match action {
            ServiceAction::Install => installed::install(&home),
            ServiceAction::Uninstall => installed::uninstall(&home),
            ServiceAction::Status => installed::status(&home),
            ServiceAction::Run => unreachable!("handled in main"),
        },
        (Role::Frontend, Some(Command::Ask { .. })) => unreachable!("handled in main"),
        (Role::Frontend, Some(Command::Displays)) => list_displays(),
        (Role::Frontend, Some(Command::Codecs)) => {
            list_codecs();
            Ok(())
        }
        (Role::Frontend, Some(Command::Fingerprint)) => {
            let ep = endpoint(&home)?;
            println!("{}", ep.identity.fingerprint);
            Ok(())
        }
        (Role::Frontend, Some(Command::List)) => {
            let ep = endpoint(&home)?;
            let store = ep.store.lock().await;
            println!("Pinned hosts (this device connects to):");
            for (address, host) in store.known_hosts() {
                println!(
                    "  {address:<28} {:<20} {}",
                    host.name,
                    &host.fingerprint[..16]
                );
            }
            println!("Paired devices (may connect to this device):");
            for (fingerprint, peer) in store.paired_peers() {
                println!(
                    "  {:<20} last from {:<24} {}",
                    peer.name,
                    peer.last_address,
                    &fingerprint[..16]
                );
            }
            Ok(())
        }
        (Role::Frontend, Some(Command::Forget { address })) => {
            let ep = endpoint(&home)?;
            let address = normalize_target(&address);
            if ep.store.lock().await.forget_host(&address)? {
                println!("Forgot the pinned key for {address}.");
            } else {
                println!("No pinned key for {address}.");
            }
            Ok(())
        }
    }
}

/// An error with its causes, e.g. "network error: connection reset".
fn describe(err: &dyn std::error::Error) -> String {
    let mut text = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

fn parse_keyboard(s: &str) -> std::result::Result<KeyboardMode, String> {
    s.parse()
}

fn parse_pointer(s: &str) -> std::result::Result<PointerMode, String> {
    s.parse()
}

/// What the host offers: its screen (or the test pattern), input injection and layout reports.
/// With the test pattern there is no real screen, so input isn't injected either.
fn host_config(test_pattern: bool, no_audio: bool, no_clipboard: bool) -> HostConfig {
    host_config_with(test_pattern, no_audio, no_clipboard, false)
}

/// What this machine can offer a controller, less what was switched off. `no_input`: the
/// controller sees the screen and cannot use the keyboard or the mouse.
fn host_config_with(
    test_pattern: bool,
    no_audio: bool,
    no_clipboard: bool,
    no_input: bool,
) -> HostConfig {
    #[cfg(windows)]
    let (input, layout, obstacle): (
        Option<InputFactory>,
        Option<LayoutFn>,
        Option<ObstacleFn>,
    ) = if test_pattern {
        (None, None, None)
    } else {
        let input: InputFactory = Arc::new(|display| {
            let displays = illum_win::list_displays().map_err(|e| e.to_string())?;
            let info = displays
                .into_iter()
                .nth(display as usize)
                .ok_or_else(|| format!("no display {display}"))?;
            Ok(Box::new(illum_win::WinInjector::new(&info)) as Box<dyn InputSink + Send>)
        });
        let layout: LayoutFn = Arc::new(illum_win::foreground_keyboard_layout);
        let obstacle: ObstacleFn = Arc::new(illum_win::input_obstacle);
        (Some(input), Some(layout), Some(obstacle))
    };
    #[cfg(not(windows))]
    let (input, layout, obstacle) = (None, None, None);
    let mut codecs: Vec<_> = illum_ffmpeg::probe()
        .encoders
        .iter()
        .map(|e| e.codec)
        .collect();
    codecs.dedup();
    // Sound: the test pattern hums a 440 Hz tone; the real host sends what it plays.
    let audio: Option<AudioFactory> = if no_audio {
        None
    } else if test_pattern {
        Some(Arc::new(|| {
            Ok(Box::new(illum_media::audio::ToneSource::default()) as Box<dyn AudioSource>)
        }))
    } else {
        #[cfg(windows)]
        {
            Some(Arc::new(|| {
                Ok(Box::new(illum_win::LoopbackCapture::new()?) as Box<dyn AudioSource>)
            }))
        }
        #[cfg(not(windows))]
        {
            None
        }
    };
    let input = if no_input { None } else { input };
    HostConfig {
        screen: Some(source_factory(test_pattern)),
        input,
        layout,
        obstacle,
        codecs,
        audio,
        // The test pattern is a non-interactive stream; the real host syncs the clipboard (§8).
        clipboard: !test_pattern && !no_clipboard,
        displays: display_names(test_pattern),
        sas: secure_attention(test_pattern || no_input),
        idle: None,
        lock: lock_host(test_pattern || no_input),
    }
}

/// Locking this machine for a controller that has the keyboard here.
fn lock_host(off: bool) -> Option<illum::host::SasFn> {
    #[cfg(windows)]
    if !off {
        return Some(Arc::new(illum_win::lock_workstation));
    }
    let _ = off;
    None
}

/// Ctrl+Alt+Del for a controller: only the host an installed service started can ask for it
/// (it asks that service, which is allowed to; architecture §7).
fn secure_attention(off: bool) -> Option<illum::host::SasFn> {
    #[cfg(windows)]
    if !off && illum_win::runs_as_system() {
        return Some(Arc::new(illum_win::request_sas));
    }
    let _ = off;
    None
}

/// The displays a controller can choose between, each as its size ("2560 × 1440, main").
fn display_names(test_pattern: bool) -> Option<illum::host::DisplaysFn> {
    if test_pattern {
        return None;
    }
    #[cfg(windows)]
    {
        Some(Arc::new(|| {
            illum_win::list_displays()
                .map(|displays| {
                    displays
                        .iter()
                        .map(|d| {
                            let main = if d.primary { ", main" } else { "" };
                            format!("{} × {}{main}", d.width, d.height)
                        })
                        .collect()
                })
                .unwrap_or_default()
        }))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// What the FFmpeg probe found on this machine, with FFmpeg's own reasons for each encoder it
/// could not open (e.g. an NVIDIA driver too old for this build's NVENC).
fn list_codecs() {
    illum_ffmpeg::set_verbose(true);
    let p = illum_ffmpeg::probe();
    illum_ffmpeg::set_verbose(false);
    println!("Encoders (best first per codec):");
    if p.encoders.is_empty() {
        println!("  none — the screen will be sent as lossless tiles");
    }
    for e in &p.encoders {
        let kind = if e.hardware { "hardware" } else { "software" };
        println!("  {:<6} {:<12} {kind}", e.codec.name(), e.name);
    }
    println!("Decoders (CPU):");
    for (codec, name) in &p.decoders {
        println!("  {:<6} {name}", codec.name());
    }
    println!("Decoders (GPU):");
    illum_ffmpeg::set_verbose(true);
    let gpu = illum_ffmpeg::hardware_decoders();
    illum_ffmpeg::set_verbose(false);
    if gpu.is_empty() {
        println!("  none — AV1 will not be offered as a viewer; H.265 decodes on the CPU");
    }
    for (codec, name) in gpu {
        println!("  {:<6} {name}", codec.name());
    }
}

/// Where the host's picture comes from: the real screen, or the colour test pattern.
fn source_factory(test_pattern: bool) -> SourceFactory {
    Arc::new(
        move |display| -> std::result::Result<Box<dyn ScreenSource>, MediaError> {
            if test_pattern {
                return Ok(Box::new(PatternSource::new(1920, 1080, 60)));
            }
            #[cfg(windows)]
            {
                Ok(Box::new(illum_win::ScreenCapture::new(display as usize)?))
            }
            #[cfg(not(windows))]
            {
                let _ = display;
                Err(MediaError::Capture(
                    "screen capture is only implemented on Windows so far; use --test-pattern"
                        .into(),
                ))
            }
        },
    )
}

fn list_displays() -> Result<()> {
    #[cfg(windows)]
    {
        for d in illum_win::list_displays()? {
            let primary = if d.primary { " (primary)" } else { "" };
            println!(
                "{}: {} {}x{} at {},{}{primary}",
                d.index, d.name, d.width, d.height, d.x, d.y
            );
        }
        Ok(())
    }
    #[cfg(not(windows))]
    bail!("listing displays is only implemented on Windows so far")
}

async fn run_service(home: &Path, port: u16, bind_to: &[IpAddr], config: HostConfig) -> Result<()> {
    // Physical pixels everywhere, matching what Desktop Duplication captures.
    #[cfg(windows)]
    illum_win::make_dpi_aware();
    let mut ep = endpoint(home)?;
    let listener = open_listener(&mut ep, &config, bind_to, port).await?;
    println!(
        "Device \"{}\" fingerprint: {}",
        ep.device_name, ep.identity.fingerprint
    );
    for addr in listener.local_addrs() {
        println!("Listening on {addr}");
    }
    accept_loop(ep, listener, config, Sessions::new(|_| {})).await;
    Ok(())
}

/// The desks connected to this one now: shown by the app, and where the person at this
/// desk ends one.
#[derive(Clone)]
struct Sessions {
    live: Arc<std::sync::Mutex<Vec<Live>>>,
    touched: Arc<
        std::sync::Mutex<
            std::collections::HashMap<String, std::sync::Weak<std::sync::atomic::AtomicU64>>,
        >,
    >,
    /// Told the names, in the order they connected, whenever one comes or goes.
    changed: Arc<dyn Fn(Vec<String>) + Send + Sync>,
}

struct Live {
    id: u64,
    name: String,
    /// Where it connected from, without a port.
    address: String,
    /// Unix seconds when it connected.
    since: u64,
    end: Arc<tokio::sync::Notify>,
    /// Ended by the person at this desk, not by the controller or the link.
    dismissed: Arc<std::sync::atomic::AtomicBool>,
}

/// One session's place among [`Sessions`]; leaving it is the session's end.
struct Entered {
    sessions: Sessions,
    id: u64,
    end: Arc<tokio::sync::Notify>,
    dismissed: Arc<std::sync::atomic::AtomicBool>,
}

impl Sessions {
    fn new(changed: impl Fn(Vec<String>) + Send + Sync + 'static) -> Sessions {
        Sessions {
            live: Arc::default(),
            touched: Arc::default(),
            changed: Arc::new(changed),
        }
    }

    /// The connected desks, each once: a desk with a display popped out has several
    /// sessions here and is still one desk.
    fn names(&self) -> Vec<String> {
        self.guests().into_iter().map(|(name, _, _)| name).collect()
    }

    /// When input last came from the desk at `address` (`now_us`), shared by all its
    /// sessions here: the idle rule counts the desk, not each of its windows.
    fn touched(&self, address: &str) -> Arc<std::sync::atomic::AtomicU64> {
        let mut touched = self.touched.lock().expect("sessions");
        touched.retain(|_, weak| weak.strong_count() > 0);
        if let Some(shared) = touched.get(address).and_then(std::sync::Weak::upgrade) {
            return shared;
        }
        let fresh = Arc::new(std::sync::atomic::AtomicU64::new(
            illum_media::clock::now_us(),
        ));
        touched.insert(address.to_string(), Arc::downgrade(&fresh));
        fresh
    }

    /// The connected desks as the app lists them: name, address, connected since.
    fn guests(&self) -> Vec<(String, String, u64)> {
        let live = self.live.lock().expect("sessions");
        let mut desks: Vec<(String, String, u64)> = Vec::new();
        for session in live.iter() {
            // The first session of a desk is the one listed: its further ones are windows
            // of the same visit.
            if !desks
                .iter()
                .any(|(name, address, _)| *name == session.name && *address == session.address)
            {
                desks.push((session.name.clone(), session.address.clone(), session.since));
            }
        }
        desks
    }

    fn enter(&self, name: &str, address: &str) -> Entered {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let entered = Entered {
            sessions: self.clone(),
            id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            end: Arc::default(),
            dismissed: Arc::default(),
        };
        self.live.lock().expect("sessions").push(Live {
            id: entered.id,
            name: name.to_string(),
            address: address.to_string(),
            since: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            end: entered.end.clone(),
            dismissed: entered.dismissed.clone(),
        });
        (self.changed)(self.names());
        entered
    }

    /// Ends every session of the desk called `name`. Returns how many there were.
    fn end(&self, name: &str) -> usize {
        let live = self.live.lock().expect("sessions");
        let theirs: Vec<&Live> = live.iter().filter(|s| s.name == name).collect();
        for session in &theirs {
            session
                .dismissed
                .store(true, std::sync::atomic::Ordering::SeqCst);
            // Kept until the session looks, should it be busy right now.
            session.end.notify_one();
        }
        theirs.len()
    }
}

impl Entered {
    fn dismissed(&self) -> bool {
        self.dismissed.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for Entered {
    fn drop(&mut self) {
        self.sessions
            .live
            .lock()
            .expect("sessions")
            .retain(|s| s.id != self.id);
        (self.sessions.changed)(self.sessions.names());
    }
}

/// Opens the listening port for a host that offers what `config` has. Narrows `ep`'s
/// capabilities to match and adds the codecs this machine can encode.
async fn open_listener(
    ep: &mut Endpoint,
    config: &HostConfig,
    bind_to: &[IpAddr],
    port: u16,
) -> Result<Listener> {
    ep.capabilities.retain(|c| match c.as_str() {
        "screen" => config.screen.is_some(),
        "input" => config.input.is_some(),
        "audio" => config.audio.is_some(),
        "clipboard" => config.clipboard,
        "sas" => config.sas.is_some(),
        "lock" => config.lock.is_some(),
        _ => true,
    });
    // "enc:av1" etc.: the viewer's hello says what it decodes, ours what we encode (§5.3).
    ep.capabilities.extend(illum_ffmpeg::probe().capabilities());
    let addrs = if bind_to.is_empty() {
        bind::tailscale_addresses()
    } else {
        bind_to.to_vec()
    };
    if addrs.is_empty() {
        bail!(
            "no Tailscale address found on this machine. Start Tailscale, or listen elsewhere with --bind <IP>"
        );
    }
    Listener::bind(ep, &addrs, port)
        .await
        .context("opening the listening port")
}

/// Whether a connection ended before the peer said anything at all (no TLS hello).
fn left_unspoken(err: &illum_net::NetError) -> bool {
    match err {
        illum_net::NetError::Io(io) => matches!(
            io.kind(),
            std::io::ErrorKind::UnexpectedEof
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
        ),
        illum_net::NetError::Closed => true,
        _ => false,
    }
}

/// Accepts connections and serves each as a session, until the listener closes or this future
/// is dropped (sessions already running carry on).
async fn accept_loop(ep: Endpoint, mut listener: Listener, config: HostConfig, sessions: Sessions) {
    while let Some((tcp, peer)) = listener.next().await {
        let ep = ep.clone();
        let acceptor = listener.acceptor();
        let config = config.clone();
        let media = listener.media();
        let sessions = sessions.clone();
        tokio::spawn(async move {
            tracing::debug!(target: "service", "[{peer}] Incoming connection.");
            match accept(&ep, &acceptor, tcp, peer).await {
                Ok(conn) => {
                    let (name, version) = conn
                        .peer_hello
                        .as_ref()
                        .map(|h| (h.device_name.clone(), h.app_version.clone()))
                        .unwrap_or_default();
                    tracing::info!(target: "service", "[{peer}] Session with \"{name}\" (Illumination {version}) started.");
                    let guest = conn.peer_fingerprint.as_str().to_string();
                    ep.store.lock().await.guest_entered(&guest);
                    let from = peer.ip().to_string();
                    let touched = sessions.touched(&from);
                    let entered = sessions.enter(&name, &from);
                    let ended = serve_session(
                        conn,
                        config,
                        ep.timeouts.idle,
                        media,
                        entered.end.clone(),
                        touched,
                    )
                    .await;
                    {
                        // A desk accepted for this time may come back for a while, unless the
                        // person here sent it away.
                        let mut store = ep.store.lock().await;
                        if entered.dismissed() {
                            store.dismiss_guest(&guest);
                        } else {
                            store.guest_left(&guest);
                        }
                    }
                    match ended {
                        Ok(()) if entered.dismissed() => {
                            tracing::info!(target: "service", "[{peer}] Session ended by the person at this desk.")
                        }
                        Ok(()) => tracing::info!(target: "service", "[{peer}] Session ended."),
                        Err(err) => {
                            tracing::warn!(target: "service", "[{peer}] Session ended: {}", describe(&err))
                        }
                    }
                    drop(entered);
                }
                // Left before saying anything: another desk's app looking whether this one
                // is there (its "online" mark), every few seconds. Not worth a line each.
                Err(err) if left_unspoken(&err) => {
                    tracing::debug!(target: "service", "[{peer}] Looked in and left.")
                }
                Err(err) => {
                    tracing::warn!(target: "service", "[{peer}] Not connected: {}", describe(&err))
                }
            }
        });
    }
}

struct ViewArgs {
    keyboard: KeyboardMode,
    pointer: PointerMode,
    view_only: bool,
    /// Present on the GPU (false: `--software-render`).
    gpu: bool,
    /// Restrict what is offered to the host (`--codec`).
    codec: Option<CodecChoice>,
    /// Close the window after this long (`--duration`).
    duration: Option<Duration>,
    /// Write the per-second records here when the window closes (`--report`).
    report: Option<PathBuf>,
    label: Option<String>,
    /// A display popped out of another session window: the picture alone.
    popout: bool,
    /// The session window this one belongs to.
    parent: Option<u32>,
    /// `--home`, to hand on to the windows this one opens.
    home_flag: Option<PathBuf>,
    params: ViewParams,
}

/// The command line of a window showing `display` of the same host, popped out of this
/// session: the same limits and the same permission, and no sound or clipboard of its own
/// (the main window has those).
fn pop_out_args(target: &str, args: &ViewArgs, display: u32) -> Vec<std::ffi::OsString> {
    let p = &args.params;
    let mut line: Vec<std::ffi::OsString> = vec![
        "view".into(),
        target.into(),
        "--screen".into(),
        display.to_string().into(),
        "--popout".into(),
        "--parent".into(),
        std::process::id().to_string().into(),
        "--mute".into(),
        "--no-clipboard".into(),
        "--keyboard".into(),
        match args.keyboard {
            KeyboardMode::Remote => "remote",
            KeyboardMode::Local => "local",
            KeyboardMode::Auto => "auto",
        }
        .into(),
        "--pointer".into(),
        args.pointer.to_string().into(),
        "--max-bitrate".into(),
        (f64::from(p.max_bitrate_kbps) / 1000.0).to_string().into(),
        "--max-fps".into(),
        p.max_fps.to_string().into(),
        "--latency-limit".into(),
        p.latency_limit_ms.to_string().into(),
    ];
    let mut flag = |on: bool, name: &str| {
        if on {
            line.push(name.into());
        }
    };
    flag(args.view_only, "--view-only");
    flag(p.scale_rungs, "--allow-scale");
    flag(!p.reconnect, "--no-reconnect");
    flag(!p.hw_decode, "--software-decode");
    flag(!args.gpu, "--software-render");
    if let Some(home) = &args.home_flag {
        line.push("--home".into());
        line.push(home.into());
    }
    line
}

/// Connects, then shows the host's screen in a window until it is closed.
fn run_view(
    runtime: &tokio::runtime::Runtime,
    home: &Path,
    target: &str,
    args: ViewArgs,
) -> Result<()> {
    let mut ep = endpoint(home)?;
    println!("This device's fingerprint: {}", ep.identity.fingerprint);
    // Tell the host which lossy codecs we can decode (AV1 only in hardware); it picks AV1,
    // else H.265, else tiles. `--codec` narrows the offer for measurement runs. The GPU
    // probe's answer is kept in the home folder for a week: on a two-GPU laptop it took
    // several seconds before every connection.
    illum_ffmpeg::set_probe_cache_dir(own_folder(home));
    let decode_caps = illum_ffmpeg::decode_capabilities(args.params.hw_decode);
    let offered: Vec<String> = match args.codec {
        None | Some(CodecChoice::Av1) => decode_caps,
        Some(CodecChoice::Hevc) => decode_caps
            .into_iter()
            .filter(|c| c == "dec:hevc")
            .collect(),
        Some(CodecChoice::Tiles) => Vec::new(),
    };
    ep.capabilities.extend(offered);
    if !args.params.clipboard {
        ep.capabilities.retain(|c| c != "clipboard");
    }
    println!("Connecting to {}…", normalize_target(target));
    let conn = runtime.block_on(connect(&ep, target))?;
    let hello = conn.peer_hello.clone().unwrap_or_else(|| Hello::ours(""));
    if !hello.capabilities.iter().any(|c| c == "screen") {
        bail!(
            "\"{}\" does not offer screen sharing (it may run an older version)",
            hello.device_name
        );
    }
    println!(
        "Connected to \"{}\" at {}.",
        hello.device_name, conn.peer_address
    );

    tracing::info!(target: "session", "Connected to \"{}\" (Illumination {}) at {}; it offers {:?}.", hello.device_name, hello.app_version, conn.peer_address, hello.capabilities);
    let with_input = !args.view_only && hello.capabilities.iter().any(|c| c == "input");
    if !args.view_only && !with_input {
        println!("The host does not accept input; opening view-only.");
    }

    let window = ViewerWindow::new().map_err(anyhow::Error::msg)?;
    let waker = window.waker();
    let shared: Shared = Arc::default();
    #[cfg(windows)]
    {
        shared.lock().expect("view state lock").local_layout =
            Some(illum_win::own_keyboard_layout());
    }
    let started_unix = illum_ui::measure::unix_now().as_secs();
    if args.report.is_some() {
        shared.lock().expect("view state lock").recorder = Some(illum_ui::Recorder::default());
    }
    if let Some(duration) = args.duration {
        let waker = waker.clone();
        runtime.spawn(async move {
            tokio::time::sleep(duration).await;
            waker.wake(UiEvent::Exit);
        });
    }
    // A popped-out display follows the window it came from: gone with it, however it went.
    if let Some(parent) = args.parent {
        let waker = waker.clone();
        std::thread::spawn(move || {
            #[cfg(windows)]
            while illum_win::process_alive(parent) {
                std::thread::sleep(Duration::from_secs(1));
            }
            #[cfg(not(windows))]
            let _ = parent;
            #[cfg(windows)]
            waker.wake(UiEvent::Exit);
            #[cfg(not(windows))]
            drop(waker);
        });
    }
    // All host-bound sends from the window go through this; the reconnect loop points it at the
    // current connection (§5.5).
    let live = illum::viewer::LiveSender::default();
    live.set(Some(conn.sender()));
    let media_slot = MediaSlot::default();
    // Shared with the window: pause the screen while minimized, and tell a static host from a
    // dead path (§5.4).
    let flags = Arc::new(illum::viewer::MediaFlags::default());
    // The windows of other displays popped out of this one, to close them with it; and the
    // command line that opens one.
    let popped: Arc<std::sync::Mutex<Vec<std::process::Child>>> = Arc::default();
    let pop_out_line = {
        let base = pop_out_args(target, &args, 0);
        Arc::new(move |number: u32| {
            let mut line = base.clone();
            line[3] = number.to_string().into(); // after "view <target> --screen"
            line
        })
    };
    flags
        .display
        .store(args.params.display, std::sync::atomic::Ordering::Relaxed);
    // Clipboard both ways when both sides offer it (§8); a copy here is sent through whichever
    // connection is live, the host's applied. Kept across reconnections, hence the Arc.
    let clipboard = if args.params.clipboard && hello.capabilities.iter().any(|c| c == "clipboard")
    {
        let live = live.clone();
        illum::clipboard::ClipboardSync::start(move |text| {
            live.send(Channel::Clipboard, ClipboardMsg::Text(text).to_bytes());
        })
        .map(Arc::new)
    } else {
        None
    };
    // The session runs in a loop: an unexpected drop, with reconnect on (the default), re-dials
    // the host and resumes — keeping the window, clipboard, input and media slot — with a
    // backoff. A clean end (the host said goodbye) stops it (§5.5).
    let session = {
        let (shared, waker) = (shared.clone(), waker.clone());
        let redraw = waker.clone();
        let notify: Notify = Arc::new(move || redraw.wake(UiEvent::Redraw));
        let params = args.params.clone();
        let (media_slot, flags, live, clipboard) = (
            media_slot.clone(),
            flags.clone(),
            live.clone(),
            clipboard.clone(),
        );
        let (ep, target) = (ep.clone(), target.to_string());
        let reconnect = args.params.reconnect;
        runtime.spawn(async move {
            let mut next = Some(conn);
            let mut backoff = Duration::from_secs(1);
            let result = loop {
                let conn = match next.take() {
                    Some(c) => c,
                    None => {
                        shared.lock().expect("view state lock").status =
                            Some("connection lost; reconnecting…".into());
                        notify();
                        match connect(&ep, &target).await {
                            Ok(c) => {
                                backoff = Duration::from_secs(1);
                                shared.lock().expect("view state lock").status = None;
                                notify();
                                c
                            }
                            Err(_) => {
                                tokio::time::sleep(backoff).await;
                                backoff = (backoff * 2).min(Duration::from_secs(10));
                                continue;
                            }
                        }
                    }
                };
                live.set(Some(conn.sender()));
                let r = run_viewer_session_sharing_media(
                    conn,
                    params.clone(),
                    shared.clone(),
                    notify.clone(),
                    media_slot.clone(),
                    clipboard.clone(),
                    flags.clone(),
                )
                .await;
                live.set(None);
                *media_slot.lock().expect("media slot") = None;
                match r {
                    Ok(()) => break Ok(()),
                    Err(e) if reconnect => {
                        tracing::warn!(target: "media", "Connection ended ({e}); reconnecting.");
                    }
                    Err(e) => break Err(e),
                }
            };
            waker.wake(UiEvent::Exit);
            result
        })
    };

    let input = with_input.then(|| {
        // Each event's departure is noted, so the host's acknowledgement becomes "input N ms".
        let (input_live, sent) = (live.clone(), shared.clone());
        let out = Arc::new(InputOut::new(move |ev| {
            sent.lock()
                .expect("view state lock")
                .input_sent(false, ev.seq);
            input_live.send(Channel::Input, ev.encode());
        }));
        // Mouse moves take the UDP path while it is up (§6); the reliable path otherwise.
        let sent = shared.clone();
        out.set_unreliable(Some(Box::new(move |ev| {
            match media_slot.lock().expect("media slot").as_ref() {
                Some(client) => {
                    sent.lock()
                        .expect("view state lock")
                        .input_sent(true, ev.seq);
                    client.send_input(&ev.encode());
                    true
                }
                None => false,
            }
        })));
        out
    });
    // The keyboard goes to the host while the viewer has focus: a hook on its own thread takes
    // every key (remote mode) or the ones Windows keeps for itself (Win, Alt+Tab, …) (§6).
    let hotkeys = Arc::new(illum_ui::GrabHotkeys::default());
    #[cfg(windows)]
    let _hook = match &input {
        Some(input) => {
            let (input, hotkeys, waker) = (input.clone(), hotkeys.clone(), waker.clone());
            let front_waker = waker.clone();
            let forward = move |key, down| match hotkeys.key(key, down) {
                illum_ui::Grabbed::Host => input.key(key, down),
                illum_ui::Grabbed::ToggleView => waker.wake(UiEvent::ToggleView),
                illum_ui::Grabbed::Viewer => {}
            };
            // The hook also watches whether this program is in front, and says so: the window
            // acts on that, not on its own focus events (§6).
            let front = move |front| front_waker.wake(UiEvent::Front(front));
            match illum_win::SystemKeyHook::install(forward, front) {
                Ok(hook) => {
                    // Remote (physical-key) mode grabs every key for the host; local/auto grab
                    // only the system keys and leave typing to this machine's layout/IME (§6).
                    illum_win::SystemKeyHook::set_grab(matches!(
                        args.keyboard,
                        KeyboardMode::Remote
                    ));
                    Some(hook)
                }
                Err(e) => {
                    println!(
                        "Note: Win, Alt+Tab and similar keys will stay on this machine ({e})."
                    );
                    None
                }
            }
        }
        None => None,
    };
    #[cfg(windows)]
    let hooked = _hook.is_some();
    #[cfg(not(windows))]
    let hooked = false;
    let keyboard_grabbed = hooked && matches!(args.keyboard, KeyboardMode::Remote);
    // Said out loud, and in the log, so a report of "the keys act here" can be read against
    // which build ran and what it did with the keyboard.
    let keyboard_note = match (with_input, keyboard_grabbed) {
        (false, _) => "not sent (view only)",
        (true, true) => "every key goes to the host while this window has focus",
        (true, false) => "typed here; only Win, Alt+Tab and the like go to the host",
    };
    println!(
        "Illumination {}. Keyboard: {keyboard_note}.",
        env!("CARGO_PKG_VERSION")
    );
    tracing::info!(target: "input", "Illumination {}; keyboard: {keyboard_note}.", env!("CARGO_PKG_VERSION"));
    let ack = live.clone();
    let mode = if with_input { "" } else { " (view only)" };
    let state_for_report = shared.clone();
    let (focus_input, focus_state) = (input.clone(), shared.clone());
    let acks_at_focus = std::sync::atomic::AtomicU64::new(0);
    window
        .run(
            shared,
            ViewerOptions {
                title: format!("Illumination — {}{mode}", hello.device_name),
                on_presented: Box::new(move |seq| ack.send_control(&ControlMsg::VideoAck { seq })),
                input,
                keyboard: args.keyboard,
                keyboard_grabbed,
                host_name: hello.device_name.clone(),
                host_address: illum_ui::app::shown_address(&normalize_target(target)).to_string(),
                // Changing the mode in the window moves the grab with it: every key in remote
                // mode, only the system keys otherwise (§6).
                on_keyboard: Box::new(move |mode| {
                    let grab = hooked && mode == KeyboardMode::Remote;
                    #[cfg(windows)]
                    illum_win::SystemKeyHook::set_grab(grab);
                    grab
                }),
                front_watched: hooked,
                pointer: args.pointer,
                on_focus: Box::new(move |focused| {
                    if focused {
                        let acks = focus_state.lock().expect("view state lock").input_acks;
                        acks_at_focus.store(acks, std::sync::atomic::Ordering::Relaxed);
                        if let Some(input) = &focus_input {
                            input.take_tally(); // what was sent without focus is not this period's
                        }
                        tracing::info!(target: "input", "Focus came.");
                    } else {
                        hotkeys.reset();
                        // What this focus period sent and what the host acknowledged: enough to
                        // tell "never left this machine" from "the host took it and nothing
                        // happened there". Counts only.
                        if let Some(input) = &focus_input {
                            let t = input.take_tally();
                            let (acks, last) = {
                                let state = focus_state.lock().expect("view state lock");
                                (state.input_acks, state.last_input_ack)
                            };
                            let acked = acks - acks_at_focus.load(std::sync::atomic::Ordering::Relaxed);
                            let last = last.map_or("never".to_string(), |at| format!("{} ms ago", at.elapsed().as_millis()));
                            tracing::info!(target: "input", "Focus left: sent {} key events, {} texts, {} clicks, {} wheels, {} moves, {} other; the host acknowledged {acked} (last {last}); the window itself saw {} key presses and {} releases.", t.keys, t.texts, t.buttons, t.wheels, t.moves, t.other, t.window_presses, t.window_releases);
                        }
                        // How the grab did while the window had focus: counts, never the keys.
                        #[cfg(windows)]
                        {
                            let (taken, left) = illum_win::SystemKeyHook::take_counts();
                            tracing::info!(target: "input", "Focus left: {taken} key presses went to the host, {left} stayed on this machine.");
                        }
                    }
                }),
                // Minimized: ask the host to pause the screen (audio, input and the connection
                // stay). The media loop also reads the flag so it does not alarm (§5.4).
                on_minimize: {
                    let (live, flags) = (live.clone(), flags.clone());
                    Box::new(move |minimized| {
                        flags
                            .screen_paused
                            .store(minimized, std::sync::atomic::Ordering::Relaxed);
                        live.send_control(&ControlMsg::ScreenPause { paused: minimized });
                    })
                },
                on_sas: {
                    let live = live.clone();
                    Box::new(move || live.send_control(&ControlMsg::SecureAttention))
                },
                can_sas: with_input && hello.capabilities.iter().any(|c| c == "sas"),
                on_lock: {
                    let live = live.clone();
                    Box::new(move || live.send_control(&ControlMsg::LockHost))
                },
                can_lock: with_input && hello.capabilities.iter().any(|c| c == "lock"),
                popped_out: args.popout,
                on_pop_out: {
                    let (popped, line_for) = (popped.clone(), pop_out_line.clone());
                    Box::new(move |number| {
                        let Ok(exe) = std::env::current_exe() else {
                            return;
                        };
                        let mut command = std::process::Command::new(exe);
                        command
                            .args(line_for(number))
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null());
                        #[cfg(windows)]
                        {
                            use std::os::windows::process::CommandExt;
                            command.creation_flags(0x0800_0000); // no console window
                        }
                        match command.spawn() {
                            Ok(child) => popped.lock().expect("popped-out windows").push(child),
                            Err(e) => {
                                tracing::warn!(target: "session", "Display {} not popped out: {e}", number + 1)
                            }
                        }
                    })
                },
                // Another of the host's displays: the screen is started anew for it, as it is
                // for a stuck media path, and the choice outlives a reconnection.
                on_display: {
                    let (live, flags, params) = (live.clone(), flags.clone(), args.params.clone());
                    Box::new(move |display| {
                        flags
                            .display
                            .store(display, std::sync::atomic::Ordering::Relaxed);
                        live.send_control(&params.screen_start(display));
                    })
                },
                gpu: args.gpu,
            },
        )
        .map_err(anyhow::Error::msg)?;

    // The displays popped out of this window close with it.
    for mut window in popped.lock().expect("popped-out windows").drain(..) {
        let _ = window.kill();
    }
    // Window closed (or the session ended): say goodbye on whichever connection is live and
    // wait briefly for a clean close.
    live.send_control(&ControlMsg::ScreenStop);
    live.send_control(&ControlMsg::Bye);
    if let Some(path) = &args.report {
        let mut state = state_for_report.lock().expect("view state lock");
        let seconds = state.recorder.take().map(|r| r.seconds).unwrap_or_default();
        let p = &args.params;
        let report = illum::measure::Report {
            label: args.label.clone().unwrap_or_else(|| {
                path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            }),
            viewer: device_name(),
            host: hello.device_name.clone(),
            started_unix,
            codec: state.codec.map_or("none", |c| c.name()).to_string(),
            decoder: state.decoder_name.clone().unwrap_or_default(),
            presenter: state.presenter_name.clone().unwrap_or_default(),
            settings: illum::measure::Settings {
                bitrate_kbps: p.max_bitrate_kbps,
                fps: p.max_fps,
                fec_min: p.fec_min,
                fec_max: p.fec_max,
                latency_limit_ms: p.latency_limit_ms,
                scale_percent: p.scale_percent,
                fixed: p.fixed,
                hw_decode: p.hw_decode,
                gpu_render: args.gpu,
                audio: p.audio,
                duration_s: args.duration.map_or(0, |d| d.as_secs()),
            },
            seconds,
        };
        drop(state);
        report
            .write(path)
            .with_context(|| format!("writing {}", path.display()))?;
        println!(
            "Report: {} ({} seconds recorded, {} with {}).",
            path.display(),
            report.seconds.len(),
            report.codec,
            report.decoder
        );
    }
    // The timer must be created inside the runtime, hence the async block.
    match runtime.block_on(async { tokio::time::timeout(Duration::from_secs(3), session).await }) {
        Ok(Ok(Ok(()))) | Err(_) => Ok(()),
        Ok(Ok(Err(err))) => Err(err.into()),
        Ok(Err(join)) => Err(anyhow::anyhow!("viewer task failed: {join}")),
    }
}

async fn run_connect(home: &Path, target: &str, pings: u32) -> Result<()> {
    let ep = endpoint(home)?;
    println!("This device's fingerprint: {}", ep.identity.fingerprint);
    println!("Connecting to {}…", normalize_target(target));
    let mut conn = connect(&ep, target).await?;
    let name = conn
        .peer_hello
        .as_ref()
        .map(|h| h.device_name.clone())
        .unwrap_or_default();
    println!(
        "Connected to \"{name}\" at {} (key {}).",
        conn.peer_address, conn.peer_fingerprint
    );

    let mut samples = Vec::new();
    for seq in 1..=u64::from(pings) {
        let rtt = conn.ping(seq, Duration::from_secs(5)).await?;
        println!("  ping {seq}: {:.2} ms", rtt.as_secs_f64() * 1000.0);
        samples.push(rtt.as_secs_f64() * 1000.0);
        if seq < u64::from(pings) {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    if !samples.is_empty() {
        let min = samples.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = samples.iter().cloned().fold(0.0, f64::max);
        let avg = samples.iter().sum::<f64>() / samples.len() as f64;
        println!("RTT min/avg/max: {min:.2} / {avg:.2} / {max:.2} ms");
    }
    conn.close(Duration::from_secs(1)).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> std::result::Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("illum").chain(args.iter().copied()))
    }

    #[test]
    fn no_flag_is_frontend() {
        assert_eq!(parse(&[]).unwrap().role(), Role::Frontend);
    }

    #[test]
    fn role_flags() {
        assert_eq!(parse(&["--service"]).unwrap().role(), Role::Service);
        assert_eq!(parse(&["--backend"]).unwrap().role(), Role::Backend);
        assert_eq!(parse(&["--control"]).unwrap().role(), Role::Control);
    }

    #[test]
    fn roles_are_exclusive_and_unknown_flags_rejected() {
        assert!(parse(&["--service", "--backend"]).is_err());
        assert!(parse(&["--nope"]).is_err());
    }

    #[test]
    fn service_options_need_service() {
        assert!(parse(&["--port", "9000"]).is_err());
        let cli = parse(&["--service", "--port", "9000", "--bind", "127.0.0.1"]).unwrap();
        assert_eq!(cli.port, 9000);
        assert_eq!(cli.bind, vec!["127.0.0.1".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn view_and_test_pattern() {
        let cli = parse(&["view", "laptop", "--screen", "1"]).unwrap();
        assert!(
            matches!(cli.command, Some(Command::View { ref target, screen: 1, .. }) if target == "laptop")
        );
        assert!(
            parse(&["--test-pattern"]).is_err(),
            "test pattern needs --service"
        );
        assert!(
            parse(&["--service", "--test-pattern"])
                .unwrap()
                .test_pattern
        );
    }

    #[test]
    fn connect_subcommand() {
        let cli = parse(&["connect", "100.100.101.1", "--pings", "3"]).unwrap();
        assert!(
            matches!(cli.command, Some(Command::Connect { ref target, pings: 3 }) if target == "100.100.101.1")
        );
    }

    /// Another desk's app knocking to see whether this one is there is told apart from a
    /// connection that failed: it connected and left without a word.
    #[test]
    fn a_knock_is_not_a_failed_connection() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let home = std::env::temp_dir().join(format!("illum-knock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let ep = endpoint(&home).unwrap();
        runtime.block_on(async {
            let mut listener = illum_net::Listener::bind(&ep, &["127.0.0.1".parse().unwrap()], 0)
                .await
                .unwrap();
            let address = listener.local_addrs()[0];
            // The knock: connect, say nothing, leave.
            drop(tokio::net::TcpStream::connect(address).await.unwrap());
            let (tcp, peer) = listener.next().await.unwrap();
            let refused = accept(&ep, &listener.acceptor(), tcp, peer)
                .await
                .err()
                .expect("nothing was said");
            assert!(left_unspoken(&refused), "{refused:?}");
            // Something that speaks, but not our language, is a failed connection.
            use tokio::io::AsyncWriteExt;
            let mut stranger = tokio::net::TcpStream::connect(address).await.unwrap();
            stranger.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
            let (tcp, peer) = listener.next().await.unwrap();
            let refused = accept(&ep, &listener.acceptor(), tcp, peer)
                .await
                .err()
                .expect("not a desk");
            assert!(!left_unspoken(&refused), "{refused:?}");
        });
        let _ = std::fs::remove_dir_all(&home);
    }

    /// What a session window starts a popped-out display with is a command line this
    /// program accepts, and it carries the display, the permission and the limits.
    #[test]
    fn a_pop_out_command_line_parses() {
        use clap::Parser;
        let args = ViewArgs {
            keyboard: KeyboardMode::Local,
            pointer: PointerMode::Host,
            view_only: true,
            gpu: false,
            codec: None,
            duration: None,
            report: None,
            label: None,
            popout: false,
            parent: None,
            home_flag: Some(PathBuf::from(r"C:\Some Home")),
            params: ViewParams {
                max_bitrate_kbps: 7_500,
                max_fps: 60,
                latency_limit_ms: 80,
                scale_rungs: true,
                reconnect: false,
                hw_decode: false,
                ..ViewParams::new(0)
            },
        };
        let mut line: Vec<std::ffi::OsString> = vec!["illum".into()];
        line.extend(pop_out_args("100.64.0.20", &args, 2));
        assert_eq!(line[4], "2", "the display is where the opener replaces it");
        let cli = Cli::try_parse_from(&line).unwrap_or_else(|e| panic!("{line:?} is refused: {e}"));
        let Some(Command::View {
            screen,
            popout,
            parent,
            view_only,
            mute,
            no_clipboard,
            max_fps,
            ..
        }) = cli.command
        else {
            panic!("not a view command");
        };
        assert_eq!(
            (screen, popout, parent),
            (2, true, Some(std::process::id()))
        );
        assert!(
            view_only && mute && no_clipboard,
            "the main window's permission, no sound of its own"
        );
        assert_eq!(max_fps, 60);
        assert_eq!(cli.home.as_deref(), Some(Path::new(r"C:\Some Home")));
    }
}
