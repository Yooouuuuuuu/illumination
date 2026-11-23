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

use crate::{Sessions, accept_loop, endpoint, host_config_with, open_listener};
use anyhow::{Context, Result, bail};
use illum_net::{Answer, Prompt, PromptKind, PromptRequest, bind};
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
/// `illum ask` exits with this when the person accepted and the desk is to be remembered.
pub const ASK_ACCEPTED: u8 = 0;
pub const ASK_DECLINED: u8 = 1;
/// Accepted for this time only.
pub const ASK_ONCE: u8 = 2;
/// Declined, and never to be asked about again.
pub const ASK_NEVER: u8 = 3;

/// What `illum ask` said by how it ended. Anything unexpected (closed, crashed) is a no.
fn answer_of(code: Option<i64>) -> Answer {
    match code {
        Some(c) if c == i64::from(ASK_ACCEPTED) => Answer::Always,
        Some(c) if c == i64::from(ASK_ONCE) => Answer::Once,
        Some(c) if c == i64::from(ASK_NEVER) => Answer::Never,
        _ => Answer::No,
    }
}

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
    /// The same, with where each is and since when (Unix seconds).
    pub guests: Vec<GuestStatus>,
    pub version: String,
    /// An update the app asked for, as it goes.
    pub update: crate::update::UpdateStatus,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuestStatus {
    pub name: String,
    pub address: String,
    pub since: u64,
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

/// What a connected desk may do besides seeing the screen, as the settings say: control,
/// sound, clipboard. A change here makes the host listen anew, offering what is left.
fn offered(settings: &illum_ui::app::Settings) -> (bool, bool, bool, u32) {
    (
        settings.let_control,
        settings.let_sound,
        settings.let_clipboard,
        settings.idle_minutes,
    )
}

/// The idle rule as the settings say it.
pub(crate) fn idle_limit(minutes: u32) -> Option<Duration> {
    (minutes > 0).then(|| Duration::from_secs(u64::from(minutes) * 60))
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
    // The list is filled a line below, once the registry it reads exists.
    let guests_of: Arc<Mutex<Option<Sessions>>> = Arc::default();
    let (shown, read) = (told.clone(), guests_of.clone());
    let sessions = Sessions::new(move |names| {
        let guests = read
            .lock()
            .expect("sessions")
            .as_ref()
            .map(|s| s.guests())
            .unwrap_or_default();
        shown.change(|s| {
            s.sessions = names;
            s.guests = guests
                .into_iter()
                .map(|(name, address, since)| GuestStatus {
                    name,
                    address,
                    since,
                })
                .collect();
        })
    });
    *guests_of.lock().expect("sessions") = Some(sessions.clone());
    // Updating, when the app asks: what an earlier update downloaded is not needed now.
    crate::update::clean(home);
    let (ending, shown) = (sessions.clone(), told.clone());
    let installer = crate::update::Installer::new(
        home,
        move || ending.end_all(illum::host::ENDED_FOR_UPDATE),
        move |update| shown.change(|s| s.update = update),
    );
    // What the app may not do itself, it asks for here (`control`).
    let ending = sessions.clone();
    let _asked = AbortOnDrop(tokio::spawn(crate::control::serve(
        home.to_path_buf(),
        crate::control::pipe_name(home, crate::is_usual_home(home)),
        crate::control::Hands {
            end: Arc::new(move |name: &str| ending.end(name)),
            connected: {
                let sessions = sessions.clone();
                Arc::new(move || sessions.names())
            },
            update: Some(installer),
        },
    )));
    // One endpoint for as long as the host runs: the desks accepted "this time" live in it.
    let base = loop {
        match endpoint(home) {
            Ok(ep) => break ep,
            Err(e) => {
                tracing::warn!(target: "service", "Not ready: {e:#}");
                told.change(|s| {
                    s.incoming = "failed".into();
                    s.why = format!("{e:#}");
                });
                tokio::time::sleep(HOST_RETRY).await;
            }
        }
    };
    loop {
        let settings = crate::app::load_settings(home);
        let offer = offered(&settings);
        // Probing the encoders can take a moment the first time; not on the async threads.
        let (control, sound, clipboard, idle_minutes) = offer;
        base.store.lock().await.set_new_desks(settings.new_desks);
        let probed = tokio::task::spawn_blocking(move || {
            host_config_with(false, !sound, !clipboard, !control)
        })
        .await;
        let config = match probed {
            Ok(mut config) => {
                config.idle = idle_limit(idle_minutes);
                config
            }
            Err(e) => {
                told.change(|s| {
                    s.incoming = "failed".into();
                    s.why = e.to_string();
                });
                return;
            }
        };
        let listening = async {
            let mut ep = base.clone();
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
        tracing::info!(target: "service", "Listening on {:?} (control {control}, sound {sound}, clipboard {clipboard}).", listener.local_addrs());
        told.change(|s| {
            s.incoming = "listening".into();
            s.why.clear();
        });
        let accepting = tokio::spawn(accept_loop(ep, listener, config, sessions.clone()));
        // Until the person switches incoming off or changes what a desk may do, or the
        // machine's addresses change (the tailnet came up late, or went). Sessions already
        // running carry on either way.
        loop {
            tokio::time::sleep(HOST_LOOK).await;
            let now = crate::app::load_settings(home);
            // Who is asked about takes effect at once, without listening anew.
            base.store.lock().await.set_new_desks(now.new_desks);
            if accepting.is_finished()
                || offered(&now) != offer
                || listen_addresses(bind_to) != addresses
            {
                break;
            }
        }
        accepting.abort();
        let _ = accepting.await;
    }
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
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
                return Answer::No; // the host never dials out
            }
            let gave_up = Arc::new(AtomicBool::new(false));
            let _guard = GaveUp(gave_up.clone());
            tokio::task::spawn_blocking(move || ask(&req, home_flag.as_deref(), &gave_up))
                .await
                .unwrap_or(Answer::No)
        })
    })
}

/// One argument of a Windows command line, quoted so that it arrives as it is.
fn quoted(arg: &str) -> String {
    // The values passed here (paths, names, addresses, fingerprints) carry no quote of their
    // own that could end the argument early: any is replaced.
    format!("\"{}\"", arg.replace('"', "'").trim_end_matches('\\'))
}

fn ask(req: &PromptRequest, home_flag: Option<&Path>, gave_up: &AtomicBool) -> Answer {
    let Ok(exe) = std::env::current_exe() else {
        return Answer::No;
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
    if !req.os.is_empty() {
        args.push("--os".to_string());
        args.push(req.os.clone());
    }
    if let Some(home) = home_flag {
        args.push("--home".to_string());
        args.push(home.display().to_string());
    }
    tracing::info!(target: "service", "Asking the person at this desk about \"{}\" at {}.", req.name, req.address);
    let answer = ask_in_window(&exe, &args, gave_up);
    tracing::info!(target: "service", "\"{}\" at {}: {}.", req.name, req.address, match answer {
        Some(Answer::Always) => "accepted",
        Some(Answer::Once) => "accepted for this time",
        Some(Answer::No) => "declined",
        Some(Answer::Never) => "declined for good",
        None => "nobody could be asked",
    });
    answer.unwrap_or(Answer::No)
}

/// Runs the question's window and waits for it. `None` when it could not be shown.
#[cfg(windows)]
fn ask_in_window(exe: &Path, args: &[String], gave_up: &AtomicBool) -> Option<Answer> {
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
                return Some(answer_of(Some(i64::from(code))));
            }
            if gave_up.load(Ordering::SeqCst) {
                window.kill();
                return Some(Answer::No);
            }
        }
    }
    ask_as_child(exe, args, gave_up)
}

#[cfg(not(windows))]
fn ask_in_window(exe: &Path, args: &[String], gave_up: &AtomicBool) -> Option<Answer> {
    ask_as_child(exe, args, gave_up)
}

/// The same window as an ordinary child: a host started by hand already runs as the person.
fn ask_as_child(exe: &Path, args: &[String], gave_up: &AtomicBool) -> Option<Answer> {
    let mut window = std::process::Command::new(exe)
        .args(args)
        .stdin(std::process::Stdio::null())
        .spawn()
        .ok()?;
    loop {
        match window.try_wait() {
            Ok(Some(status)) => return Some(answer_of(status.code().map(i64::from))),
            Ok(None) => {}
            Err(_) => return None,
        }
        if gave_up.load(Ordering::SeqCst) {
            let _ = window.kill();
            return Some(Answer::No);
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
        // Started by an update's installer: the app was open, and opens again.
        let home = crate::default_home();
        let reopen = crate::update::take_reopen(&home);
        if reopen {
            reopen_app(&exe);
        }
        // Started by Windows at boot, with "Start on boot" switched off: not wanted until
        // the person opens the app, which starts this service saying so.
        let by_app =
            reopen || illum_win::service_start_args().iter().any(|a| a == STARTED_BY_APP);
        if !by_app && !crate::app::load_settings(&crate::default_home()).start_on_boot {
            tracing::info!(target: "service", "\"Start on boot\" is off: stopping until the app is opened.");
            return;
        }
        let mut host: Option<illum_win::SessionProcess> = None;
        let mut last_start: Option<std::time::Instant> = None;
        // Ctrl+Alt+Del for a controller: the host raises a flag, this service makes the
        // call that only a service may make.
        let sas = match illum_win::SasGate::open() {
            Ok(gate) => Some(gate),
            Err(why) => {
                tracing::warn!(target: "service", "No Ctrl+Alt+Del for controllers: {why}");
                None
            }
        };
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
            match &sas {
                Some(gate) => {
                    if gate.asked(Duration::from_millis(500)) {
                        match illum_win::send_sas() {
                            Ok(()) => {
                                tracing::info!(target: "service", "Ctrl+Alt+Del sent for a controller.")
                            }
                            Err(why) => {
                                tracing::warn!(target: "service", "Ctrl+Alt+Del not sent: {why}")
                            }
                        }
                    }
                }
                None => std::thread::sleep(Duration::from_millis(500)),
            }
        }
        if let Some(running) = host {
            running.kill();
            running.wait(Duration::from_secs(5));
        }
        tracing::info!(target: "service", "Service stopped.");
    })
    .map_err(anyhow::Error::msg)
}

/// Opens the app as the person at the screen, after an update replaced it.
#[cfg(windows)]
fn reopen_app(exe: &Path) {
    let line = quoted(&exe.display().to_string());
    match illum_win::console_session()
        .map(|session| illum_win::start_as_session_user(session, &line))
    {
        Some(Ok(_)) => tracing::info!(target: "service", "Updated: the app is opened again."),
        Some(Err(why)) => {
            tracing::warn!(target: "service", "Updated, but the app was not opened again: {why}")
        }
        None => {
            tracing::warn!(target: "service", "Updated, but nobody is at the screen to open the app for.")
        }
    }
}

#[cfg(not(windows))]
pub fn run_service() -> Result<()> {
    bail!("the installed service exists on Windows only so far")
}

/// Runs a Windows tool and returns its exit code with what it printed.
#[cfg(windows)]
fn tool(program: &str, args: &[&str]) -> Result<(i32, String)> {
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW: also run from the windowed app, where a console would flash up.
    let out = std::process::Command::new(program)
        .args(args)
        .creation_flags(0x0800_0000)
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

/// What the app passes when it starts the service, to tell its start from one at boot.
pub const STARTED_BY_APP: &str = "app";

/// Starts the installed service for the app that was just opened, if it is installed and
/// not running, and waits for it to be up. `false` when there is no service to start.
#[cfg(windows)]
pub fn start_for_app() -> bool {
    use illum_win::{SERVICE_NAME, ServiceState, service_state};
    match service_state() {
        ServiceState::NotInstalled => false,
        ServiceState::Running => true,
        ServiceState::Stopped => {
            let _ = tool("sc.exe", &["start", SERVICE_NAME, STARTED_BY_APP]);
            wait_until(|| service_state() == ServiceState::Running);
            // Its host needs a moment more to open the pipe the app talks to.
            std::thread::sleep(Duration::from_millis(500));
            service_state() == ServiceState::Running
        }
    }
}

/// Stops the installed service, as closing the app does. Needs no administrator: installing
/// lets the PC's users start and stop this one service.
#[cfg(windows)]
pub fn stop_for_app() {
    use illum_win::{SERVICE_NAME, ServiceState, service_state};
    if service_state() == ServiceState::Running {
        let _ = tool("sc.exe", &["stop", SERVICE_NAME]);
        wait_until(|| service_state() != ServiceState::Running);
    }
}

/// Makes the app start with the person's sign-in, in the tray, or not.
#[cfg(windows)]
pub fn set_autostart(on: bool) {
    const RUN: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    let done = if on {
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let exe = exe.display().to_string();
        let exe = exe.strip_prefix(r"\\?\").unwrap_or(&exe).to_string();
        let command = format!("\"{exe}\" --tray");
        tool(
            "reg.exe",
            &[
                "add",
                RUN,
                "/v",
                "Illumination",
                "/t",
                "REG_SZ",
                "/d",
                &command,
                "/f",
            ],
        )
    } else {
        tool("reg.exe", &["delete", RUN, "/v", "Illumination", "/f"])
    };
    if let Err(e) = done {
        tracing::warn!(target: "app", "Start on boot not set: {e:#}");
    }
}

#[cfg(not(windows))]
pub fn start_for_app() -> bool {
    false
}

#[cfg(not(windows))]
pub fn stop_for_app() {}

#[cfg(not(windows))]
pub fn set_autostart(_on: bool) {}

/// Windows' own name for "not allowed": the command needs an administrator.
#[cfg(windows)]
const ACCESS_DENIED: i32 = 5;
#[cfg(windows)]
const FIREWALL_RULE: &str = "name=Illumination";
#[cfg(windows)]
const SAS_POLICY_KEY: &str = r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System";
#[cfg(windows)]
const SAS_POLICY: &str = "SoftwareSASGeneration";

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
    close_home(home)?;
    if let Err(why) = allow_sas() {
        // Not fatal: everything else works without.
        println!("Ctrl+Alt+Del from a controller will not work: {why:#}");
    }
    // The PC's users may start and stop this service (and nothing else about it): closing
    // the app stops it and opening the app starts it, without an administrator's prompt.
    // (IU: whoever is signed in at the PC. RP start, WP stop, added to what users have.)
    must(
        "letting users start and stop the service",
        "sc.exe",
        &[
            "sdset",
            SERVICE_NAME,
            "D:(A;;CCLCSWRPWPDTLOCRRC;;;SY)(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)\
             (A;;CCLCSWRPWPLOCRRC;;;IU)(A;;CCLCSWLOCRRC;;;SU)",
        ],
    )?;
    must("starting the service", "sc.exe", &["start", SERVICE_NAME])?;
    wait_until(|| service_state() == ServiceState::Running);
    println!(
        "Illumination is installed as a service and {}.",
        match service_state() {
            ServiceState::Running => "running",
            _ => "not running yet (see logs\\service-run.<date>.log in the home folder)",
        }
    );
    Ok(())
}

/// What the policy has to become so that services may ask for the Ctrl+Alt+Del screen,
/// given what it is (`None`: not set). The value is two switches: 1 for services, 2 for
/// accessibility programs. `None`: it is right already.
#[cfg(windows)]
fn sas_policy_wanted(now: Option<u32>) -> Option<u32> {
    match now {
        Some(value) if value & 1 == 1 => None,
        Some(value) => Some(value | 1),
        None => Some(1),
    }
}

/// Lets services ask Windows for the Ctrl+Alt+Del screen, which is how a controller gets
/// it (architecture §7). Other remote-desktop programs set the same policy, so it is only
/// ever widened here and left alone when uninstalling.
#[cfg(windows)]
fn allow_sas() -> Result<()> {
    let (code, said) = tool("reg.exe", &["query", SAS_POLICY_KEY, "/v", SAS_POLICY])?;
    // "    SoftwareSASGeneration    REG_DWORD    0x1"
    let now = (code == 0)
        .then(|| said.rsplit("0x").next().map(str::trim).map(str::to_string))
        .flatten()
        .and_then(|hex| u32::from_str_radix(&hex, 16).ok());
    let Some(wanted) = sas_policy_wanted(now) else {
        return Ok(());
    };
    must(
        "allowing Ctrl+Alt+Del for services",
        "reg.exe",
        &[
            "add",
            SAS_POLICY_KEY,
            "/v",
            SAS_POLICY,
            "/t",
            "REG_DWORD",
            "/d",
            &wanted.to_string(),
            "/f",
        ],
    )
}

/// Makes the home folder the system's: the system and administrators write it, the PC's
/// users read it. The host has system rights and believes what is in there (which desks
/// get in without asking), so no ordinary account may write it; the app asks the host
/// instead (`control`). Files made before the install, by whoever ran the app then, are
/// taken over too: their maker could otherwise open them up again.
#[cfg(windows)]
fn close_home(home: &Path) -> Result<()> {
    std::fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;
    let home_text = home.display().to_string();
    // Administrators: S-1-5-32-544. The system: S-1-5-18. Users: S-1-5-32-545.
    must(
        "taking over the home folder",
        "icacls.exe",
        &[&home_text, "/setowner", "*S-1-5-32-544", "/T", "/C", "/Q"],
    )?;
    must(
        "closing the home folder",
        "icacls.exe",
        &[
            &home_text,
            "/inheritance:r",
            "/grant:r",
            "*S-1-5-18:(OI)(CI)F",
            "*S-1-5-32-544:(OI)(CI)F",
            "*S-1-5-32-545:(OI)(CI)RX",
            "/Q",
        ],
    )?;
    // Everything inside takes the folder's list and nothing of its own.
    let inside = format!(r"{home_text}\*");
    // (Nothing inside yet, on a first install, is "file not found" to icacls.)
    let _ = tool("icacls.exe", &[&inside, "/reset", "/T", "/C", "/Q"]);
    Ok(())
}

/// Gives the home folder back to the PC's users, for running the app without the service.
#[cfg(windows)]
fn open_home(home: &Path) {
    if !home.is_dir() {
        return;
    }
    let home_text = home.display().to_string();
    let _ = tool(
        "icacls.exe",
        &[&home_text, "/grant", "*S-1-5-32-545:(OI)(CI)M", "/Q"],
    );
}

/// Stops and removes the service and its firewall rule. Keys, desks and settings stay.
#[cfg(windows)]
pub fn uninstall(home: &Path) -> Result<()> {
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
    open_home(home);
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
pub fn uninstall(_home: &Path) -> Result<()> {
    bail!("the installed service exists on Windows only so far")
}

#[cfg(not(windows))]
pub fn status(_home: &Path) -> Result<()> {
    bail!("the installed service exists on Windows only so far")
}

/// The pipe on which this program asks the installed host for changes to `home`, if that is
/// how it has to make them: the service runs, `home` is the folder it serves, and this is
/// not the host itself.
pub fn control_pipe(home: &Path) -> Option<String> {
    #[cfg(windows)]
    if illum_win::runs_as_system() {
        return None;
    }
    (crate::is_usual_home(home) && service_runs()).then(|| crate::control::pipe_name(home, true))
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
    fn the_window_answers_by_how_it_ends() {
        assert_eq!(answer_of(Some(0)), Answer::Always);
        assert_eq!(answer_of(Some(2)), Answer::Once);
        assert_eq!(answer_of(Some(1)), Answer::No);
        assert_eq!(answer_of(Some(3)), Answer::Never);
        // Closed from the title bar, ended by the task manager, crashed.
        for other in [None, Some(-1), Some(4), Some(0xC000_0005)] {
            assert_eq!(answer_of(other), Answer::No);
        }
    }

    #[cfg(windows)]
    #[test]
    fn the_sas_policy_is_only_widened() {
        assert_eq!(sas_policy_wanted(None), Some(1));
        assert_eq!(sas_policy_wanted(Some(0)), Some(1));
        assert_eq!(
            sas_policy_wanted(Some(2)),
            Some(3),
            "another program's switch stays"
        );
        assert_eq!(sas_policy_wanted(Some(1)), None);
        assert_eq!(sas_policy_wanted(Some(3)), None);
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
