// SPDX-License-Identifier: GPL-3.0-only
//! `illum`: one binary, every role (frontend, service, backend, control).
//!
//! Runs in a console for now: `illum --service` hosts, `illum view <ip>` opens a viewer window,
//! `illum connect <ip>` measures round-trip time. The Windows service, the backend and the
//! full app window arrive in later milestones.

mod prompt;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use illum::host::{HostConfig, SourceFactory, serve_session};
#[cfg(windows)]
use illum::host::{InputFactory, LayoutFn};
use illum::viewer::{Notify, ViewParams, run_viewer_session};
use illum_core::Role;
use illum_core::input::InputMsg;
#[cfg(windows)]
use illum_core::input::InputSink;
use illum_core::protocol::Channel;
use illum_core::protocol::DEFAULT_PORT;
use illum_core::protocol::{ControlMsg, Hello};
use illum_core::store::TrustStore;
use illum_media::MediaError;
use illum_media::source::{PatternSource, ScreenSource};
use illum_net::session::{accept, connect, normalize_target};
use illum_net::{Endpoint, Identity, Listener, Timeouts, bind};
use illum_ui::{InputOut, KeyboardMode, Shared, UiEvent, ViewerOptions, ViewerWindow};
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
        /// How keys are sent: remote (key positions; the host's layout and IME apply),
        /// local (characters as typed here), or auto (remote when both layouts match).
        #[arg(long, default_value = "auto", value_parser = parse_keyboard)]
        keyboard: KeyboardMode,
        /// Only watch: send no keyboard or mouse input.
        #[arg(long)]
        view_only: bool,
        /// Cap on the host's media rate in Mb/s (the top of the ladder; default 20).
        #[arg(long, default_value_t = 20.0)]
        max_bitrate: f64,
    },
    /// List this machine's displays.
    Displays,
    /// Show which video codecs this machine can encode and decode (AV1, H.265).
    Codecs,
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
fn default_home() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(|p| PathBuf::from(p).join("Illumination"))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".illumination")))
        .unwrap_or_else(|| PathBuf::from(".illumination"))
}

fn device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown device".into())
}

fn endpoint(home: &Path) -> Result<Endpoint> {
    let identity = Identity::load_or_create(home).context("loading the device key")?;
    let store = TrustStore::load(home.join("trust.json")).context("loading the trust store")?;
    Ok(Endpoint {
        identity: Arc::new(identity),
        store: Arc::new(Mutex::new(store)),
        device_name: device_name(),
        timeouts: Timeouts::default(),
        prompt: prompt::console_prompt(),
        capabilities: illum_net::session::all_capabilities(),
    })
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_target(true)
        .with_writer(std::io::stderr)
        // Colour codes only where they are understood: classic Windows consoles print them raw.
        .with_ansi(!cfg!(windows) && std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_max_level(if cli.verbose {
            tracing::Level::DEBUG
        } else {
            tracing::Level::INFO
        })
        .init();
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
            view_only,
            max_bitrate,
        }) if cli.role() == Role::Frontend => {
            let home = cli.home.clone().unwrap_or_else(default_home);
            let options = ViewArgs {
                screen: *screen,
                keyboard: *keyboard,
                view_only: *view_only,
                max_bitrate_kbps: (max_bitrate.max(0.1) * 1000.0) as u32,
            };
            run_view(&runtime, &home, target, options)
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
        (Role::Service, _) => {
            run_service(&home, cli.port, &cli.bind, host_config(cli.test_pattern)).await
        }
        (Role::Backend | Role::Control, _) => bail!("role {role:?} is not implemented yet"),
        (Role::Frontend, None) => {
            println!("The app window isn't built yet. Try `illum connect <ip>` or `illum --help`.");
            Ok(())
        }
        (Role::Frontend, Some(Command::Connect { target, pings })) => {
            run_connect(&home, &target, pings).await
        }
        (Role::Frontend, Some(Command::View { .. })) => unreachable!("handled in main"),
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

/// What the host offers: its screen (or the test pattern), input injection and layout reports.
/// With the test pattern there is no real screen, so input isn't injected either.
fn host_config(test_pattern: bool) -> HostConfig {
    #[cfg(windows)]
    let (input, layout): (Option<InputFactory>, Option<LayoutFn>) = if test_pattern {
        (None, None)
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
        (Some(input), Some(layout))
    };
    #[cfg(not(windows))]
    let (input, layout) = (None, None);
    let mut codecs: Vec<_> = illum_ffmpeg::probe()
        .encoders
        .iter()
        .map(|e| e.codec)
        .collect();
    codecs.dedup();
    HostConfig {
        screen: Some(source_factory(test_pattern)),
        input,
        layout,
        codecs,
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
    println!("Decoders:");
    for (codec, name) in &p.decoders {
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
                Ok(Box::new(illum_win::DdaSource::new(display as usize)?))
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
    ep.capabilities.retain(|c| match c.as_str() {
        "screen" => config.screen.is_some(),
        "input" => config.input.is_some(),
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
    let mut listener = Listener::bind(&ep, &addrs, port)
        .await
        .context("opening the listening port")?;
    println!(
        "Device \"{}\" fingerprint: {}",
        ep.device_name, ep.identity.fingerprint
    );
    for addr in listener.local_addrs() {
        println!("Listening on {addr}");
    }
    while let Some((tcp, peer)) = listener.next().await {
        let ep = ep.clone();
        let acceptor = listener.acceptor();
        let config = config.clone();
        let media = listener.media();
        tokio::spawn(async move {
            tracing::info!(target: "service", "[{peer}] Incoming connection.");
            match accept(&ep, &acceptor, tcp, peer).await {
                Ok(conn) => {
                    let name = conn
                        .peer_hello
                        .as_ref()
                        .map(|h| h.device_name.clone())
                        .unwrap_or_default();
                    tracing::info!(target: "service", "[{peer}] Session with \"{name}\" started.");
                    match serve_session(conn, config, ep.timeouts.idle, media).await {
                        Ok(()) => tracing::info!(target: "service", "[{peer}] Session ended."),
                        Err(err) => {
                            tracing::warn!(target: "service", "[{peer}] Session ended: {}", describe(&err))
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(target: "service", "[{peer}] Not connected: {}", describe(&err))
                }
            }
        });
    }
    Ok(())
}

struct ViewArgs {
    screen: u32,
    keyboard: KeyboardMode,
    view_only: bool,
    max_bitrate_kbps: u32,
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
    // Tell the host which lossy codecs we can decode; it picks AV1, else H.265, else tiles.
    ep.capabilities.extend(
        illum_ffmpeg::probe()
            .capabilities()
            .into_iter()
            .filter(|c| c.starts_with("dec:")),
    );
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
    let sender = conn.sender();
    let session = {
        let (shared, waker) = (shared.clone(), waker.clone());
        let redraw = waker.clone();
        let notify: Notify = Arc::new(move || redraw.wake(UiEvent::Redraw));
        let params = ViewParams {
            display: args.screen,
            max_bitrate_kbps: args.max_bitrate_kbps,
            media_target: None,
        };
        runtime.spawn(async move {
            let result = run_viewer_session(conn, params, shared, notify).await;
            waker.wake(UiEvent::Exit);
            result
        })
    };

    let input = with_input.then(|| {
        let input_sender = sender.clone();
        Arc::new(InputOut::new(move |ev| {
            input_sender.send(Channel::Input, ev.encode())
        }))
    });
    // Keys Windows keeps for itself (Win, Alt+Tab, …) go to the host while the viewer has focus.
    #[cfg(windows)]
    let _hook = match &input {
        Some(input) => {
            let input = input.clone();
            match illum_win::SystemKeyHook::install(move |key, down| {
                input.send(InputMsg::Key { key, down })
            }) {
                Ok(hook) => Some(hook),
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
    let ack = sender.clone();
    let mode = if with_input { "" } else { " (view only)" };
    window
        .run(
            shared,
            ViewerOptions {
                title: format!("Illumination — {}{mode}", hello.device_name),
                on_presented: Box::new(move |seq| ack.send_control(&ControlMsg::VideoAck { seq })),
                input,
                keyboard: args.keyboard,
                on_focus: Box::new(|focused| {
                    #[cfg(windows)]
                    illum_win::SystemKeyHook::set_active(focused);
                    #[cfg(not(windows))]
                    let _ = focused;
                }),
            },
        )
        .map_err(anyhow::Error::msg)?;

    // Window closed (or the session ended): say goodbye and wait briefly for a clean close.
    sender.send_control(&ControlMsg::ScreenStop);
    sender.send_control(&ControlMsg::Bye);
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
}
