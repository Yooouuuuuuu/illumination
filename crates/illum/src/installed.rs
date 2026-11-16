// SPDX-License-Identifier: GPL-3.0-only
//! The installed desk (architecture §3): the service, the host it keeps running in the
//! session at the screen, and what the two tell the app.
//!
//! Three processes, no pipe between them:
//! - `illum service run`, started by Windows as the system account. It only keeps the next
//!   one alive in whichever session has the physical screen.
//! - `illum --service --from-service`, the host, with system rights inside that session. It
//!   listens and serves sessions exactly as the app does when nothing is installed, and so
//!   reaches windows that run as administrator.
//! - the app, as the person. It does not listen while the service runs. It reads what the
//!   host writes to `host-status.json`, and the host reads the app's `settings.json`. A new
//!   desk's question is a small window the host starts as the person (`illum ask`); its exit
//!   code is the answer.

use crate::{SessionEvent, accept_loop, endpoint, host_config, open_listener};
use anyhow::{Context, Result, bail};
use illum_net::{Prompt, PromptKind, PromptRequest, bind};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How often the host looks at the settings and its addresses.
const HOST_LOOK: Duration = Duration::from_secs(1);
/// How long the host waits before trying again after it could not listen (no tailnet address
/// yet after a start with Windows, the port still held by an app that is closing).
const HOST_RETRY: Duration = Duration::from_secs(5);
/// `illum ask` exits with this when the person accepted.
pub const ASK_ACCEPTED: u8 = 0;
pub const ASK_DECLINED: u8 = 1;

/// What the installed host is doing, for the app to show. Written by the host on every
/// change; read by the app.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostStatus {
    /// "off", "listening" or "failed".
    pub incoming: String,
    /// Why, when it failed.
    pub why: String,
    /// The desks connected to this one now.
    pub sessions: Vec<String>,
    pub version: String,
}

pub fn status_path(home: &Path) -> PathBuf {
    home.join("host-status.json")
}

pub fn read_status(home: &Path) -> Option<HostStatus> {
    serde_json::from_str(&std::fs::read_to_string(status_path(home)).ok()?).ok()
}

fn write_status(home: &Path, status: &HostStatus) {
    let path = status_path(home);
    let tmp = path.with_extension("json.tmp");
    let written = serde_json::to_string_pretty(status)
        .map_err(std::io::Error::other)
        .and_then(|text| std::fs::write(&tmp, text))
        .and_then(|()| std::fs::rename(&tmp, &path));
    if let Err(e) = written {
        tracing::warn!(target: "service", "Could not write {}: {e}", path.display());
    }
}

/// The host's status as it changes: kept in memory, written on every change.
struct Told {
    home: PathBuf,
    status: Mutex<HostStatus>,
}

impl Told {
    fn change(&self, change: impl FnOnce(&mut HostStatus)) {
        let mut status = self.status.lock().expect("host status");
        let was = status.clone();
        change(&mut status);
        if *status != was {
            write_status(&self.home, &status);
        }
    }
}

/// The host of an installed desk: listens while the settings allow it, on the tailnet
/// addresses the machine has now, and says what it does in `host-status.json`. Runs until the
/// process is ended (by the service).
pub async fn run_host(home: &Path, home_flag: Option<PathBuf>, port: u16, bind_to: &[IpAddr]) {
    #[cfg(windows)]
    illum_win::make_dpi_aware();
    let told = Arc::new(Told {
        home: home.to_path_buf(),
        status: Mutex::new(HostStatus::default()),
    });
    // Whatever an earlier host left behind is not true any more.
    write_status(
        home,
        &HostStatus {
            incoming: "off".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            ..HostStatus::default()
        },
    );
    told.change(|s| {
        s.incoming = "off".into();
        s.version = env!("CARGO_PKG_VERSION").into();
    });
    tracing::info!(target: "service", "Illumination {} host of the installed desk.", env!("CARGO_PKG_VERSION"));
    // Probing the encoders can take a moment; once, not on the async threads.
    let config = match tokio::task::spawn_blocking(|| host_config(false, false, false)).await {
        Ok(config) => config,
        Err(e) => {
            told.change(|s| {
                s.incoming = "failed".into();
                s.why = e.to_string();
            });
            return;
        }
    };
    loop {
        if !crate::app::load_settings(home).allow_incoming {
            told.change(|s| {
                s.incoming = "off".into();
                s.why.clear();
            });
            tokio::time::sleep(HOST_LOOK).await;
            continue;
        }
        let listening = async {
            let mut ep = endpoint(home)?;
            ep.prompt = asking_prompt(home_flag.clone());
            let listener = open_listener(&mut ep, &config, bind_to, port).await?;
            anyhow::Ok((ep, listener))
        };
        let (ep, listener) = match listening.await {
            Ok(ready) => ready,
            Err(e) => {
                tracing::warn!(target: "service", "Not listening: {e:#}");
                told.change(|s| {
                    s.incoming = "failed".into();
                    s.why = format!("{e:#}");
                });
                tokio::time::sleep(HOST_RETRY).await;
                continue;
            }
        };
        let addresses = listen_addresses(bind_to);
        tracing::info!(target: "service", "Listening on {:?}.", listener.local_addrs());
        told.change(|s| {
            s.incoming = "listening".into();
            s.why.clear();
        });
        let shown = told.clone();
        let on_session = Arc::new(move |event: SessionEvent| {
            shown.change(|s| match event {
                SessionEvent::Started(name) => s.sessions.push(name),
                SessionEvent::Ended(name) => {
                    if let Some(i) = s.sessions.iter().position(|n| *n == name) {
                        s.sessions.remove(i);
                    }
                }
            })
        });
        let accepting = tokio::spawn(accept_loop(ep, listener, config.clone(), on_session));
        // Until the person switches incoming off, or the machine's addresses change (the
        // tailnet came up late, or went). Sessions already running carry on either way.
        loop {
            tokio::time::sleep(HOST_LOOK).await;
            if accepting.is_finished()
                || !crate::app::load_settings(home).allow_incoming
                || listen_addresses(bind_to) != addresses
            {
                break;
            }
        }
        accepting.abort();
        let _ = accepting.await;
    }
}

fn listen_addresses(bind_to: &[IpAddr]) -> Vec<IpAddr> {
    if bind_to.is_empty() {
        bind::tailscale_addresses()
    } else {
        bind_to.to_vec()
    }
}

/// Ends the question's window when nobody waits for the answer any more.
struct GaveUp(Arc<AtomicBool>);

impl Drop for GaveUp {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A new desk's question, asked in a window of its own that runs as the person at the screen
/// (`illum ask`). Nobody signed in, or the window closed: declined. A desk accepted before
/// is never asked about, so it connects with nobody there.
fn asking_prompt(home_flag: Option<PathBuf>) -> Prompt {
    Arc::new(move |req: PromptRequest| {
        let home_flag = home_flag.clone();
        Box::pin(async move {
            if req.kind != PromptKind::PairPeer {
                return false; // the host never dials out
            }
            let gave_up = Arc::new(AtomicBool::new(false));
            let _guard = GaveUp(gave_up.clone());
            tokio::task::spawn_blocking(move || ask(&req, home_flag.as_deref(), &gave_up))
                .await
                .unwrap_or(false)
        })
    })
}

/// One argument of a Windows command line, quoted so that it arrives as it is.
fn quoted(arg: &str) -> String {
    // The values passed here (paths, names, addresses, fingerprints) carry no quote of their
    // own that could end the argument early: any is replaced.
    format!("\"{}\"", arg.replace('"', "'").trim_end_matches('\\'))
}

fn ask(req: &PromptRequest, home_flag: Option<&Path>, gave_up: &AtomicBool) -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let mut args = vec![
        "ask".to_string(),
        "--name".to_string(),
        req.name.clone(),
        "--address".to_string(),
        req.address.clone(),
        "--fingerprint".to_string(),
        req.fingerprint.as_str().to_string(),
    ];
    if let Some(home) = home_flag {
        args.push("--home".to_string());
        args.push(home.display().to_string());
    }
    tracing::info!(target: "service", "Asking the person at this desk about \"{}\" at {}.", req.name, req.address);
    let answer = ask_in_window(&exe, &args, gave_up);
    tracing::info!(target: "service", "\"{}\" at {}: {}.", req.name, req.address, match answer {
        Some(true) => "accepted",
        Some(false) => "declined",
        None => "nobody could be asked",
    });
    answer.unwrap_or(false)
}

/// Runs the question's window and waits for it. `None` when it could not be shown.
#[cfg(windows)]
fn ask_in_window(exe: &Path, args: &[String], gave_up: &AtomicBool) -> Option<bool> {
    if illum_win::runs_as_system() {
        // As the person, not as the system: a window of ours on their desktop must not carry
        // system rights.
        let line = std::iter::once(quoted(&exe.display().to_string()))
            .chain(args.iter().map(|a| quoted(a)))
            .collect::<Vec<_>>()
            .join(" ");
        let session = illum_win::own_session()?;
        let window = match illum_win::start_as_session_user(session, &line) {
            Ok(window) => window,
            Err(why) => {
                tracing::info!(target: "service", "No question shown: {why}.");
                return None;
            }
        };
        loop {
            if let Some(code) = window.wait(Duration::from_millis(100)) {
                return Some(code == u32::from(ASK_ACCEPTED));
            }
            if gave_up.load(Ordering::SeqCst) {
                window.kill();
                return Some(false);
            }
        }
    }
    ask_as_child(exe, args, gave_up)
}

#[cfg(not(windows))]
fn ask_in_window(exe: &Path, args: &[String], gave_up: &AtomicBool) -> Option<bool> {
    ask_as_child(exe, args, gave_up)
}

/// The same window as an ordinary child: a host started by hand already runs as the person.
fn ask_as_child(exe: &Path, args: &[String], gave_up: &AtomicBool) -> Option<bool> {
    let mut window = std::process::Command::new(exe)
        .args(args)
        .stdin(std::process::Stdio::null())
        .spawn()
        .ok()?;
    loop {
        match window.try_wait() {
            Ok(Some(status)) => return Some(status.code() == Some(i32::from(ASK_ACCEPTED))),
            Ok(None) => {}
            Err(_) => return None,
        }
        if gave_up.load(Ordering::SeqCst) {
            let _ = window.kill();
            return Some(false);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The service itself: keeps one host running in the session that has the screen. Returns
/// when Windows stops the service.
#[cfg(windows)]
pub fn run_service() -> Result<()> {
    /// Between two starts of the host, so a host that cannot run does not spin.
    const BETWEEN_STARTS: Duration = Duration::from_secs(3);
    let exe = std::env::current_exe().context("finding this program")?;
    let line = format!(
        "{} --service --from-service",
        quoted(&exe.display().to_string())
    );
    illum_win::run_as_service(move |stop| {
        tracing::info!(target: "service", "Illumination {} service started.", env!("CARGO_PKG_VERSION"));
        let mut host: Option<illum_win::SessionProcess> = None;
        let mut last_start: Option<std::time::Instant> = None;
        while !stop.load(Ordering::SeqCst) {
            let screen = illum_win::console_session();
            if let Some(running) = &host {
                let ended = running.exit_code();
                if let Some(code) = ended {
                    tracing::warn!(target: "service", "The host in session {} ended (code {code}).", running.session);
                    host = None;
                } else if screen != Some(running.session) {
                    // Someone else's session has the screen now (sign-out, switch of user).
                    tracing::info!(target: "service", "The screen moved from session {} to {screen:?}.", running.session);
                    running.kill();
                    running.wait(Duration::from_secs(5));
                    host = None;
                }
            }
            if host.is_none()
                && let Some(session) = screen
                && last_start.is_none_or(|at| at.elapsed() >= BETWEEN_STARTS)
            {
                last_start = Some(std::time::Instant::now());
                match illum_win::start_in_session(session, &line) {
                    Ok(started) => {
                        tracing::info!(target: "service", "Host started in session {session}.");
                        host = Some(started);
                    }
                    Err(why) => tracing::warn!(target: "service", "Host not started: {why}."),
                }
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        if let Some(running) = host {
            running.kill();
            running.wait(Duration::from_secs(5));
        }
        tracing::info!(target: "service", "Service stopped.");
    })
    .map_err(anyhow::Error::msg)
}

#[cfg(not(windows))]
pub fn run_service() -> Result<()> {
    bail!("the installed service exists on Windows only so far")
}

/// Runs a Windows tool and returns its exit code with what it printed.
#[cfg(windows)]
fn tool(program: &str, args: &[&str]) -> Result<(i32, String)> {
    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("running {program}"))?;
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    Ok((out.status.code().unwrap_or(-1), said.trim().to_string()))
}

#[cfg(windows)]
fn must(what: &str, program: &str, args: &[&str]) -> Result<()> {
    let (code, said) = tool(program, args)?;
    if code != 0 {
        bail!("{what} failed ({program} said, code {code}): {said}");
    }
    Ok(())
}

/// Windows' own name for "not allowed": the command needs an administrator.
#[cfg(windows)]
const ACCESS_DENIED: i32 = 5;
#[cfg(windows)]
const FIREWALL_RULE: &str = "name=Illumination";

/// Registers this program as the service, opens the firewall for it and starts it. Run again
/// after the program moved or was replaced, it points the service at where it is now.
#[cfg(windows)]
pub fn install(home: &Path) -> Result<()> {
    use illum_win::{SERVICE_NAME, ServiceState, service_state};
    let exe = std::env::current_exe().context("finding this program")?;
    let exe = exe.display().to_string();
    let exe = exe.strip_prefix(r"\\?\").unwrap_or(&exe).to_string();
    let command = format!("\"{exe}\" service run");
    let existed = service_state() != ServiceState::NotInstalled;
    if existed {
        // Stopped first, so the new settings are what starts.
        let _ = tool("sc.exe", &["stop", SERVICE_NAME]);
        wait_until(|| service_state() != ServiceState::Running);
    }
    let (code, said) = if existed {
        tool(
            "sc.exe",
            &[
                "config",
                SERVICE_NAME,
                "binPath=",
                &command,
                "start=",
                "auto",
            ],
        )?
    } else {
        tool(
            "sc.exe",
            &[
                "create",
                SERVICE_NAME,
                "binPath=",
                &command,
                "start=",
                "auto",
                "DisplayName=",
                "Illumination",
            ],
        )?
    };
    if code == ACCESS_DENIED {
        bail!(
            "installing the service needs an administrator: run this from a terminal opened with \"Run as administrator\""
        );
    }
    if code != 0 {
        bail!("registering the service failed (sc.exe said, code {code}): {said}");
    }
    must(
        "describing the service",
        "sc.exe",
        &[
            "description",
            SERVICE_NAME,
            "Lets this desk be reached with Illumination: keeps its host running at the screen, \
             including windows that run as administrator.",
        ],
    )?;
    // Started again by Windows should it ever end by itself.
    must(
        "setting the service's recovery",
        "sc.exe",
        &[
            "failure",
            SERVICE_NAME,
            "reset=",
            "86400",
            "actions=",
            "restart/5000/restart/5000/restart/5000",
        ],
    )?;
    // One rule for this program, replacing whatever an earlier install left.
    let _ = tool(
        "netsh.exe",
        &["advfirewall", "firewall", "delete", "rule", FIREWALL_RULE],
    );
    let program = format!("program={exe}");
    must(
        "opening the firewall",
        "netsh.exe",
        &[
            "advfirewall",
            "firewall",
            "add",
            "rule",
            FIREWALL_RULE,
            "dir=in",
            "action=allow",
            &program,
            "enable=yes",
            "profile=any",
        ],
    )?;
    // The app runs as the person and the host as the system; both keep their files here.
    // (Users: S-1-5-32-545. See architecture §3 for what this leaves open.)
    std::fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;
    let home_text = home.display().to_string();
    must(
        "sharing the home folder",
        "icacls.exe",
        &[
            &home_text,
            "/grant",
            "*S-1-5-32-545:(OI)(CI)M",
            "/T",
            "/C",
            "/Q",
        ],
    )?;
    must("starting the service", "sc.exe", &["start", SERVICE_NAME])?;
    wait_until(|| service_state() == ServiceState::Running);
    println!(
        "Illumination is installed as a service and {}.",
        match service_state() {
            ServiceState::Running => "running",
            _ => "not running yet (see logs\\service-run.log in the home folder)",
        }
    );
    Ok(())
}

/// Stops and removes the service and its firewall rule. Keys, desks and settings stay.
#[cfg(windows)]
pub fn uninstall() -> Result<()> {
    use illum_win::{SERVICE_NAME, ServiceState, service_state};
    if service_state() == ServiceState::NotInstalled {
        println!("The Illumination service is not installed.");
        return Ok(());
    }
    let _ = tool("sc.exe", &["stop", SERVICE_NAME]);
    wait_until(|| service_state() != ServiceState::Running);
    let (code, said) = tool("sc.exe", &["delete", SERVICE_NAME])?;
    if code == ACCESS_DENIED {
        bail!(
            "removing the service needs an administrator: run this from a terminal opened with \"Run as administrator\""
        );
    }
    if code != 0 {
        bail!("removing the service failed (sc.exe said, code {code}): {said}");
    }
    let _ = tool(
        "netsh.exe",
        &["advfirewall", "firewall", "delete", "rule", FIREWALL_RULE],
    );
    println!("The Illumination service is removed. Keys, desks and settings are kept.");
    Ok(())
}

#[cfg(windows)]
fn wait_until(done: impl Fn() -> bool) {
    for _ in 0..100 {
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Says whether the service is installed and what its host reports.
#[cfg(windows)]
pub fn status(home: &Path) -> Result<()> {
    use illum_win::{ServiceState, service_state};
    match service_state() {
        ServiceState::NotInstalled => println!("Service: not installed."),
        ServiceState::Stopped => println!("Service: installed, not running."),
        ServiceState::Running => {
            println!("Service: running.");
            match read_status(home) {
                Some(s) => println!(
                    "Host (Illumination {}): {}{}; connected now: {}.",
                    s.version,
                    s.incoming,
                    if s.why.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", s.why)
                    },
                    if s.sessions.is_empty() {
                        "nobody".to_string()
                    } else {
                        s.sessions.join(", ")
                    }
                ),
                None => println!("Host: has not reported yet."),
            }
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn install(_home: &Path) -> Result<()> {
    bail!("the installed service exists on Windows only so far")
}

#[cfg(not(windows))]
pub fn uninstall() -> Result<()> {
    bail!("the installed service exists on Windows only so far")
}

#[cfg(not(windows))]
pub fn status(_home: &Path) -> Result<()> {
    bail!("the installed service exists on Windows only so far")
}

/// Whether the installed service is running, so that the app leaves the listening to it.
pub fn service_runs() -> bool {
    #[cfg(windows)]
    {
        illum_win::service_state() == illum_win::ServiceState::Running
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_survives_the_file() {
        let home = std::env::temp_dir().join(format!("illum-status-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        assert_eq!(read_status(&home), None);
        let told = Told {
            home: home.clone(),
            status: Mutex::new(HostStatus::default()),
        };
        told.change(|s| {
            s.incoming = "listening".into();
            s.sessions.push("LAPTOP".into());
        });
        let read = read_status(&home).expect("written on change");
        assert_eq!(read.incoming, "listening");
        assert_eq!(read.sessions, ["LAPTOP"]);
        // A file from a newer version, with fields this one does not know, still reads.
        std::fs::write(status_path(&home), r#"{"incoming":"off","new":1}"#).unwrap();
        assert_eq!(read_status(&home).unwrap().incoming, "off");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn arguments_are_quoted_whole() {
        assert_eq!(
            quoted("C:\\Program Files\\Illumination\\illum.exe"),
            "\"C:\\Program Files\\Illumination\\illum.exe\""
        );
        // A name cannot end its own argument and start another.
        assert_eq!(quoted("PC\" --evil"), "\"PC' --evil\"");
        // A trailing backslash would swallow the closing quote.
        assert_eq!(quoted("C:\\Home\\"), "\"C:\\Home\"");
    }
}
