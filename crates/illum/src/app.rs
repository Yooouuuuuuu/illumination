// SPDX-License-Identifier: GPL-3.0-only
//! `illum` with no command: the app. This is the half behind the window (`illum_ui::app`):
//! it keeps the window's [`AppState`] true and carries out its [`Command`]s.
//!
//! - **This desk** listens in this process while the app is open and no installed service
//!   listens, exactly as `illum --service` does, with the questions asked in the window
//!   instead of a console.
//! - **Connect** first dials from here, so a wrong address, a refusal or a changed key is said
//!   in the window. Then the session opens in
//!   its own process (`illum view …`): one window per session, and a session that crashes does
//!   not take the app or the other sessions with it. AnyDesk and RustDesk do the same.
//! - **Settings** live in `settings.json` in the home folder and become the flags of the next
//!   `illum view`.

use crate::{Sessions, accept_loop, describe, endpoint, host_config_with, open_listener};
use anyhow::{Context, Result};
use illum_core::protocol::{ControlMsg, DEFAULT_PORT};
use illum_net::session::{connect, normalize_target};
use illum_net::{Answer, Endpoint, Prompt, PromptRequest, bind};
use illum_ui::app::{
    AppState, Checked, Command, Desk, Dial, Guest, Incoming, NewRelease, Progress, Question,
    Settings, Verdict,
};
use illum_ui::app_window::AppWindow;
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

type Shared = Arc<Mutex<AppState>>;
/// Who is waiting for the answer to each question on screen.
type Answers = Arc<Mutex<HashMap<u64, oneshot::Sender<Answer>>>>;

fn settings_path(home: &Path) -> PathBuf {
    home.join("settings.json")
}

/// The saved settings; the defaults when there are none or the file cannot be read (a damaged
/// settings file must not keep the app from opening).
pub(crate) fn load_settings(home: &Path) -> Settings {
    let path = settings_path(home);
    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Settings>(&text) {
            Ok(mut settings) => {
                // Up to 0.14.5 the setting for new desks was "refuse them" or not.
                let old: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
                if old.get("new_desks").is_none() && old["refuse_new"] == true {
                    settings.new_desks = illum_ui::app::Permission::Never;
                }
                settings
            }
            Err(e) => {
                tracing::warn!(target: "app", "{} is not readable ({e}); using the defaults.", path.display());
                Settings::default()
            }
        },
        Err(_) => Settings::default(),
    }
}

pub(crate) fn save_settings(home: &Path, settings: &Settings) -> Result<()> {
    let path = settings_path(home);
    let text = serde_json::to_string_pretty(settings)?;
    // Written whole, then moved into place: a crash never leaves half a file.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// The flags of `illum view` that say what `settings` says.
fn view_flags(settings: &Settings, view_only: bool, mute: bool) -> Vec<String> {
    let mut flags = vec![
        "--max-bitrate".to_string(),
        settings.max_bitrate_mbps.to_string(),
        "--max-fps".to_string(),
        settings.max_fps.to_string(),
        "--latency-limit".to_string(),
        settings.latency_limit_ms.to_string(),
        "--tab-opacity".to_string(),
        // The window takes how much shows.
        (100 - settings.exit_tab_transparency.min(100)).to_string(),
    ];
    let mut flag = |on: bool, name: &str| {
        if on {
            flags.push(name.to_string());
        }
    };
    flag(settings.allow_scale, "--allow-scale");
    flag(!settings.reconnect, "--no-reconnect");
    flag(!settings.sound || mute, "--mute");
    flag(!settings.clipboard, "--no-clipboard");
    flag(view_only, "--view-only");
    flags
}

/// Removes a question from the screen when nobody waits for its answer any more: it was
/// answered, or the connection gave up waiting.
struct Asked {
    id: u64,
    state: Shared,
    answers: Answers,
    waker: Wake,
}

impl Drop for Asked {
    fn drop(&mut self) {
        self.answers.lock().expect("answers").remove(&self.id);
        self.state
            .lock()
            .expect("app state")
            .questions
            .retain(|q| q.id != self.id);
        (self.waker)(false);
    }
}

/// Key questions asked in the window: the question goes on screen, the answer comes back when
/// a button is pressed.
fn window_prompt(state: Shared, answers: Answers, waker: Wake) -> Prompt {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    Arc::new(move |req: PromptRequest| {
        let (state, answers, waker) = (state.clone(), answers.clone(), waker.clone());
        Box::pin(async move {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let (answer, asked) = oneshot::channel();
            answers.lock().expect("answers").insert(id, answer);
            state
                .lock()
                .expect("app state")
                .questions
                .push_back(Question {
                    id,
                    name: if req.name.is_empty() {
                        "this desk".to_string()
                    } else {
                        req.name.clone()
                    },
                    address: req.address.clone(),
                    fingerprint: req.fingerprint.as_str().to_string(),
                    os: req.os.clone(),
                });
            waker(true);
            let _on_screen = Asked {
                id,
                state,
                answers,
                waker,
            };
            asked.await.unwrap_or(Answer::No)
        })
    })
}

/// Wakes the window after the state changed; `true` also asks the person to look at it.
type Wake = Arc<dyn Fn(bool) + Send + Sync>;
/// Opens a session with a host (`target`, the `illum view` flags). The real one starts a
/// process and returns it.
type Launch = Arc<dyn Fn(&str, Vec<String>) -> Result<Option<std::process::Child>> + Send + Sync>;

/// Where this desk listens and what it shows. The app uses the tailnet addresses, the usual
/// port and the real screen; the tests a loopback port and the test pattern.
struct Hosting {
    /// Empty: this machine's tailnet addresses.
    listen_on: Vec<IpAddr>,
    port: u16,
    test_pattern: bool,
    /// Leave the listening to the installed service while it runs (the app; not the tests).
    service: bool,
    /// Set on an installed desk: the settings are saved by the host, asked on this pipe.
    control: Option<String>,
}

/// How far an update this app asked for is: not asked, asked and not yet taken up by the
/// host, or on its way there.
const IDLE: u8 = 0;
const ASKED: u8 = 1;
const FOLLOWING: u8 = 2;

struct Backend {
    home: PathBuf,
    ep: Endpoint,
    state: Shared,
    answers: Answers,
    waker: Wake,
    runtime: tokio::runtime::Handle,
    hosting: Hosting,
    launch: Launch,
    /// The task that listens for other desks, while this app does the listening itself.
    listening: Mutex<Option<JoinHandle<()>>>,
    /// The desks connected to this one, while this app does the listening itself.
    sessions: Sessions,
    /// Rung when the list of desks changed, so the new one's "online" is looked at now.
    desks_changed: tokio::sync::Notify,
    /// The session windows this app opened, each with the address it went to: to bring one
    /// to the front when that desk is asked for again, and to end them when the app closes.
    opened: Mutex<Vec<(String, Arc<Mutex<std::process::Child>>)>>,
    /// An update asked for here ([`IDLE`], [`ASKED`], [`FOLLOWING`]).
    update_phase: AtomicU8,
}

/// The command line of a session: `view <target> <flags>`, then `--home` when this app was
/// given one, so the session uses the same key and trust store. `--home` goes after the
/// subcommand: before it, the parser takes it for the app's own and refuses the subcommand.
fn session_args(
    home_flag: Option<&Path>,
    target: &str,
    flags: Vec<String>,
) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = vec!["view".into(), target.into()];
    args.extend(flags.into_iter().map(Into::into));
    if let Some(home) = home_flag {
        args.push("--home".into());
        args.push(home.into());
    }
    args
}

/// Starts `illum view` as a process of its own.
fn session_process(home_flag: Option<PathBuf>) -> Launch {
    Arc::new(move |target, flags| {
        let exe = std::env::current_exe().context("finding this program")?;
        let mut command = std::process::Command::new(exe);
        command.args(session_args(home_flag.as_deref(), target, flags));
        command
            // Ours to write on: "raise" brings the session's window to the front.
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: the session has its own window and needs no console.
            command.creation_flags(0x0800_0000);
        }
        command.spawn().map(Some).context("starting the session")
    })
}

impl Backend {
    /// `ep` is this desk's endpoint; its key questions are redirected to the window here.
    fn new(
        home: &Path,
        mut ep: Endpoint,
        waker: Wake,
        runtime: tokio::runtime::Handle,
        hosting: Hosting,
        launch: Launch,
    ) -> Arc<Backend> {
        let addresses = if hosting.listen_on.is_empty() {
            bind::tailscale_addresses()
        } else {
            hosting.listen_on.clone()
        };
        let state: Shared = Arc::new(Mutex::new(AppState {
            version: env!("CARGO_PKG_VERSION").to_string(),
            device_name: ep.device_name.clone(),
            addresses: addresses.iter().map(|a| a.to_string()).collect(),
            fingerprint: ep.identity.fingerprint.as_str().to_string(),
            settings: load_settings(home),
            ..AppState::default()
        }));
        let answers = Answers::default();
        ep.prompt = window_prompt(state.clone(), answers.clone(), waker.clone());
        Arc::new_cyclic(|this: &std::sync::Weak<Backend>| {
            let this = this.clone();
            let sessions = Sessions::new(move |names| {
                let Some(this) = this.upgrade() else {
                    return;
                };
                // A desk may have been accepted a moment ago: show it as trusted.
                let lists = this.clone();
                this.runtime
                    .spawn(async move { lists.refresh_desks().await });
                let guests = this
                    .sessions
                    .guests()
                    .into_iter()
                    .map(|(name, address, since)| Guest {
                        name,
                        address,
                        since,
                    })
                    .collect();
                this.update(|s| {
                    s.sessions = names;
                    s.guests = guests;
                });
            });
            Backend {
                home: home.to_path_buf(),
                ep,
                state,
                answers,
                waker,
                runtime,
                hosting,
                launch,
                listening: Mutex::new(None),
                sessions,
                desks_changed: tokio::sync::Notify::new(),
                opened: Mutex::new(Vec::new()),
                update_phase: AtomicU8::new(IDLE),
            }
        })
    }

    /// Fills the lists from the trust store and starts listening if the settings say so.
    fn start(self: &Arc<Self>) {
        let this = self.clone();
        self.runtime
            .spawn(async move { this.refresh_desks().await });
        let this = self.clone();
        self.runtime.spawn(async move { this.watch_desks().await });
        // "Connected for 12:40" counts on while a desk is connected.
        let this = self.clone();
        self.runtime.spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                if !this.state.lock().expect("app state").guests.is_empty() {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs());
                    this.update(|s| s.now = now);
                }
            }
        });
        // Updates: looked for a moment after the window is up, then once a day. Only the
        // app of the usual home does; a desk run with --home is a test desk.
        let installed = self.hosting.control.is_some();
        self.update(|s| s.update.installed = installed);
        if self.hosting.service {
            let this = self.clone();
            self.runtime.spawn(async move {
                tokio::time::sleep(Duration::from_secs(5)).await;
                loop {
                    this.clone().check_updates(false).await;
                    tokio::time::sleep(Duration::from_secs(24 * 60 * 60)).await;
                }
            });
        }
        // An open app listens: whether a desk gets in is its permission's matter, and not
        // being reachable at all is closing the app.
        self.set_incoming(true);
    }

    fn update(&self, change: impl FnOnce(&mut AppState)) {
        change(&mut self.state.lock().expect("app state"));
        (self.waker)(false);
    }

    /// Puts the desks the trust store knows into what the window shows: one entry per desk,
    /// whichever way it was met.
    /// Whether who may connect is locked: a desk is connected here, and could be the one
    /// asking through this desk's screen. The installed host checks the same (`control`).
    fn locked(&self) -> bool {
        !self.state.lock().expect("app state").sessions.is_empty()
    }

    async fn refresh_desks(&self) {
        let desks = {
            let mut store = self.ep.store.lock().await;
            store.refresh();
            desk_list(&store)
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let new_desks = self.state.lock().expect("app state").desks != desks;
        self.update(|s| {
            s.desks = desks;
            s.now = now;
        });
        if new_desks {
            self.desks_changed.notify_one();
        }
    }

    fn command(self: &Arc<Self>, command: Command) {
        let this = self.clone();
        match command {
            Command::Connect {
                target,
                view_only,
                mute,
            } => {
                self.runtime
                    .spawn(async move { this.dial(target, view_only, mute).await });
            }
            Command::DismissDial => self.update(|s| s.dial = Dial::Idle),
            Command::Answer { id, verdict } => {
                if let Some(answer) = self.answers.lock().expect("answers").remove(&id) {
                    let _ = answer.send(match verdict {
                        Verdict::No => Answer::No,
                        Verdict::Once => Answer::Once,
                        Verdict::Always => Answer::Always,
                        Verdict::Never => Answer::Never,
                    });
                }
            }
            Command::RaiseSessions => {
                let opened: Vec<String> = self
                    .opened
                    .lock()
                    .expect("opened sessions")
                    .iter()
                    .map(|(address, _)| address.clone())
                    .collect();
                for address in opened {
                    self.raise(&address);
                }
            }
            Command::ForgetDesk(fingerprint) => {
                if self.locked() {
                    tracing::warn!(target: "app", "Not forgetting a desk: one is connected here.");
                    return;
                }
                self.runtime.spawn(async move {
                    if let Err(e) = this.ep.store.lock().await.forget_desk(&fingerprint) {
                        tracing::warn!(target: "app", "Could not forget a desk: {e}");
                    }
                    this.refresh_desks().await;
                });
            }
            Command::Favorite { fingerprint, on } => {
                self.runtime.spawn(async move {
                    if let Err(e) = this.ep.store.lock().await.set_favorite(&fingerprint, on) {
                        tracing::warn!(target: "app", "Could not star a desk: {e}");
                    }
                    this.refresh_desks().await;
                });
            }
            Command::EditDesk {
                fingerprint,
                name,
                permission,
                favorite,
            } => {
                // The name and the star may change; the permission waits for no desk connected.
                let locked = self.locked();
                if locked {
                    tracing::info!(target: "app", "A desk's permission is not changed while one is connected here.");
                }
                self.runtime.spawn(async move {
                    let saved = {
                        let mut store = this.ep.store.lock().await;
                        store
                            .rename(&fingerprint, &name)
                            .and_then(|_| {
                                if locked {
                                    Ok(false)
                                } else {
                                    store.set_permission(&fingerprint, permission)
                                }
                            })
                            .and_then(|_| store.set_favorite(&fingerprint, favorite))
                    };
                    if let Err(e) = saved {
                        tracing::warn!(target: "app", "Could not save a desk: {e}");
                    }
                    this.refresh_desks().await;
                });
            }
            Command::SaveSettings(settings) => {
                let was = self.state.lock().expect("app state").settings.clone();
                // Who may connect stays as it was while a desk is connected here.
                let settings = if self.locked() && settings.incoming() != was.incoming() {
                    tracing::warn!(target: "app", "Incoming settings kept: a desk is connected here.");
                    settings.with_incoming_of(&was)
                } else {
                    settings
                };
                let settings_start_on_boot = settings.start_on_boot;
                let saved = match &self.hosting.control {
                    Some(pipe) => crate::control::ask(
                        pipe,
                        &crate::control::Request::Settings {
                            settings: settings.clone(),
                        },
                    )
                    .map(|_| ())
                    .map_err(anyhow::Error::msg),
                    None => save_settings(&self.home, &settings),
                };
                if let Err(e) = saved {
                    tracing::warn!(target: "app", "Settings not saved: {e:#}");
                }
                // What a connected desk may do is part of what is listened with.
                let offer_changed = (
                    settings.let_control,
                    settings.let_sound,
                    settings.let_clipboard,
                    settings.idle_minutes,
                ) != (
                    was.let_control,
                    was.let_sound,
                    was.let_clipboard,
                    was.idle_minutes,
                );
                // Who is asked about takes effect at once.
                {
                    let (this, new_desks) = (self.clone(), settings.new_desks);
                    self.runtime.spawn(async move {
                        this.ep.store.lock().await.set_new_desks(new_desks);
                    });
                }
                self.update(|s| s.settings = settings);
                if offer_changed {
                    self.set_incoming(true);
                }
                if settings_start_on_boot != was.start_on_boot && self.hosting.service {
                    crate::installed::set_autostart(settings_start_on_boot);
                }
            }
            Command::CheckUpdates => {
                self.runtime
                    .spawn(async move { this.check_updates(true).await });
            }
            Command::UpdateNow => match &self.hosting.control {
                Some(pipe) => {
                    let pipe = pipe.clone();
                    self.update_phase.store(ASKED, Ordering::SeqCst);
                    self.update(|s| s.update.progress = Progress::Downloading(None));
                    self.runtime.spawn_blocking(move || {
                        let asked = crate::control::ask(&pipe, &crate::control::Request::Update);
                        if let Err(why) = asked {
                            tracing::warn!(target: "app", "Not updated: {why}");
                            this.update_phase.store(IDLE, Ordering::SeqCst);
                            this.update(|s| s.update.progress = Progress::Failed(why));
                        }
                    });
                }
                None => self.update(|s| {
                    s.update.progress = Progress::Failed("this copy is not installed".into())
                }),
            },
            Command::CancelUpdate => {
                if let Some(pipe) = self.hosting.control.clone() {
                    self.runtime.spawn_blocking(move || {
                        let _ = crate::control::ask(&pipe, &crate::control::Request::CancelUpdate);
                    });
                }
            }
            // Handled by the window itself; nothing for the rest of the app to do.
            Command::Hide => {}
            Command::Quit => self.quit(),
            Command::EndSession(name) => match &self.hosting.control {
                Some(pipe) => {
                    let pipe = pipe.clone();
                    self.runtime.spawn_blocking(move || {
                        let asked = crate::control::ask(
                            &pipe,
                            &crate::control::Request::EndSession { name: name.clone() },
                        );
                        if let Err(e) = asked {
                            tracing::warn!(target: "app", "\"{name}\" not disconnected: {e}");
                        }
                    });
                }
                None => {
                    self.sessions.end(&name);
                }
            },
        }
    }

    /// Keeps "online" true on the desk cards: every so often each desk's port is knocked on.
    /// Nothing is said and nothing is sent; a host takes it for a connection that left
    /// before speaking.
    async fn watch_desks(self: Arc<Self>) {
        /// How often the desks are looked at, and how long one has to answer.
        const EVERY: Duration = Duration::from_secs(20);
        const PATIENCE: Duration = Duration::from_secs(2);
        loop {
            let addresses: Vec<String> = {
                let state = self.state.lock().expect("app state");
                state
                    .desks
                    .iter()
                    .filter_map(|d| d.connect_to.clone())
                    .collect()
            };
            let knocks = addresses.into_iter().map(|address| {
                tokio::spawn(async move {
                    let answered = matches!(
                        tokio::time::timeout(PATIENCE, tokio::net::TcpStream::connect(&address))
                            .await,
                        Ok(Ok(_))
                    );
                    (address, answered)
                })
            });
            let mut online = HashMap::new();
            for knock in knocks.collect::<Vec<_>>() {
                if let Ok((address, answered)) = knock.await {
                    online.insert(address, answered);
                }
            }
            if self.state.lock().expect("app state").online != online {
                self.update(|s| s.online = online);
            }
            tokio::select! {
                () = tokio::time::sleep(EVERY) => {}
                () = self.desks_changed.notified() => {}
            }
        }
    }

    /// Starts or stops listening for other desks.
    fn set_incoming(self: &Arc<Self>, on: bool) {
        let mut listening = self.listening.lock().expect("listening task");
        let before = listening.take();
        if let Some(task) = &before {
            task.abort();
        }
        if !on {
            self.update(|s| s.incoming = Incoming::Off);
            return;
        }
        if self.hosting.service && crate::installed::service_runs() {
            let this = self.clone();
            *listening = Some(
                self.runtime
                    .spawn(async move { this.follow_service().await }),
            );
            return;
        }
        let this = self.clone();
        let test_pattern = self.hosting.test_pattern;
        let (control, sound, clipboard, idle_minutes, new_desks) = {
            let state = self.state.lock().expect("app state");
            (
                state.settings.let_control,
                state.settings.let_sound,
                state.settings.let_clipboard,
                state.settings.idle_minutes,
                state.settings.new_desks,
            )
        };
        *listening = Some(self.runtime.spawn(async move {
            // The port is free once the listening before this is gone.
            if let Some(before) = before {
                let _ = before.await;
            }
            // Probing the encoders can take a moment; not on the async threads.
            this.ep.store.lock().await.set_new_desks(new_desks);
            let config = match tokio::task::spawn_blocking(move || {
                host_config_with(test_pattern, !sound, !clipboard, !control)
            })
            .await
            {
                Ok(mut config) => {
                    config.idle = crate::installed::idle_limit(idle_minutes);
                    config
                }
                Err(e) => {
                    this.update(|s| s.incoming = Incoming::Failed(e.to_string()));
                    return;
                }
            };
            let mut ep = this.ep.clone();
            match open_listener(&mut ep, &config, &this.hosting.listen_on, this.hosting.port).await
            {
                Ok(listener) => {
                    tracing::info!(target: "app", "Listening on {:?}.", listener.local_addrs());
                    this.update(|s| s.incoming = Incoming::Listening);
                    accept_loop(ep, listener, config, this.sessions.clone()).await;
                }
                Err(e) => {
                    tracing::warn!(target: "app", "Not listening: {e:#}");
                    // The usual reason has a plain answer; anything else is said as it is.
                    let in_use = e.chain().any(|cause| {
                        cause
                            .downcast_ref::<std::io::Error>()
                            .is_some_and(|io| io.kind() == std::io::ErrorKind::AddrInUse)
                    });
                    let why = if in_use {
                        format!(
                            "another Illumination is already listening on this desk (port {}). \
                             Close it, then open this app again.",
                            this.hosting.port
                        )
                    } else {
                        format!("{e:#}")
                    };
                    this.update(|s| s.incoming = Incoming::Failed(why));
                }
            }
        }));
    }

    /// Shows what the installed host does, while it does the listening: whether it listens,
    /// who is connected, and the desks it accepted. The host reads the same settings file,
    /// so what is saved here applies there.
    async fn follow_service(self: Arc<Self>) {
        tracing::info!(target: "app", "The installed service listens for this desk.");
        let mut sessions: Option<Vec<String>> = None;
        loop {
            // The update first, and whether or not the service runs: the installer stops the
            // service a moment after the host says it runs, and what the host said stays in
            // the file.
            if self.update_phase.load(Ordering::SeqCst) != IDLE
                && let Some(status) = crate::installed::read_status(&self.home)
            {
                self.follow_update(&status.update);
            }
            let incoming = if !crate::installed::service_runs() {
                Incoming::Failed(
                    "the Illumination service is not running. Close Illumination and open it again, \
                     which starts it."
                        .to_string(),
                )
            } else {
                match crate::installed::read_status(&self.home) {
                    Some(status) => {
                        if sessions.as_ref() != Some(&status.sessions) {
                            // A desk may have been accepted a moment ago: show it as trusted.
                            self.refresh_desks().await;
                            sessions = Some(status.sessions.clone());
                            let names = status.sessions.clone();
                            let guests = status
                                .guests
                                .iter()
                                .map(|g| Guest {
                                    name: g.name.clone(),
                                    address: g.address.clone(),
                                    since: g.since,
                                })
                                .collect();
                            self.update(|s| {
                                s.sessions = names;
                                s.guests = guests;
                            });
                        }
                        match status.incoming.as_str() {
                            "listening" => Incoming::Listening,
                            "failed" => Incoming::Failed(status.why),
                            _ => Incoming::Off,
                        }
                    }
                    None => Incoming::Off,
                }
            };
            if self.state.lock().expect("app state").incoming != incoming {
                self.update(|s| s.incoming = incoming);
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    /// Asks GitHub whether a newer release is out. A check nobody asked for that fails says
    /// nothing; one the person asked for says why.
    async fn check_updates(self: Arc<Self>, asked: bool) {
        let before = self.state.lock().expect("app state").update.checked.clone();
        self.update(|s| s.update.checked = Checked::Checking);
        let found = tokio::task::spawn_blocking(crate::update::check)
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        match found {
            Ok(release) => {
                match &release {
                    Some(r) => tracing::info!(target: "app", "Illumination {} is out.", r.version),
                    None => tracing::info!(target: "app", "Up to date."),
                }
                self.update(|s| {
                    s.now = now;
                    s.update.checked = Checked::Done(now);
                    // Not taken away from under an update on its way.
                    if release.is_some() || s.update.progress == Progress::Idle {
                        s.update.available = release.map(|r| NewRelease {
                            version: r.version,
                            notes: r.notes,
                        });
                    }
                });
            }
            Err(why) => {
                tracing::warn!(target: "app", "Could not check for updates: {why}");
                self.update(|s| {
                    s.update.checked = if asked { Checked::Failed(why) } else { before };
                });
            }
        }
    }

    /// What the host says of the update this app asked for. Once its installer runs, the
    /// window closes so that the program can be replaced.
    fn follow_update(&self, status: &crate::update::UpdateStatus) {
        let phase = self.update_phase.load(Ordering::SeqCst);
        if phase == IDLE || (phase == ASKED && status.state.is_empty()) {
            return;
        }
        let progress = match status.state.as_str() {
            "downloading" => Progress::Downloading(
                (status.total > 0).then(|| (status.done * 100 / status.total).min(100) as u8),
            ),
            "installing" => Progress::Installing,
            "failed" => Progress::Failed(status.why.clone()),
            _ => Progress::Idle,
        };
        self.update_phase.store(
            if matches!(progress, Progress::Idle | Progress::Failed(_)) {
                IDLE
            } else {
                FOLLOWING
            },
            Ordering::SeqCst,
        );
        let closing = progress == Progress::Installing;
        if closing {
            tracing::info!(target: "app", "The update's installer runs: closing.");
        }
        let shown = self
            .state
            .lock()
            .expect("app state")
            .update
            .progress
            .clone();
        if shown != progress || closing {
            self.update(|s| {
                s.update.progress = progress;
                s.update.closing = closing;
            });
        }
    }

    /// Reaches `target` from here (key check and errors in the window), then opens the session
    /// in its own process.
    async fn dial(self: Arc<Self>, target: String, view_only: bool, mute: bool) {
        let address = normalize_target(&target);
        let shown = illum_ui::app::shown_address(&address).to_string();
        // Desks are reached by address (architecture §4.1): a name is not looked up.
        if address.parse::<std::net::SocketAddr>().is_err() {
            self.update(|s| {
                s.dial = Dial::Failed {
                    target: target.trim().to_string(),
                    message: "Type the desk's IP address, like 100.100.101.2.".to_string(),
                }
            });
            return;
        }
        // Already open from here: that window comes to the front, and no second one opens.
        if self.raise(&address) {
            return;
        }
        {
            let mut state = self.state.lock().expect("app state");
            if matches!(state.dial, Dial::Dialing(_)) {
                return; // one at a time
            }
            state.dial = Dial::Dialing(shown.clone());
        }
        (self.waker)(false);
        let failed = |message: String| Dial::Failed {
            target: shown.clone(),
            message,
        };
        let outcome = match connect(&self.ep, &target).await {
            Ok(conn) => {
                // Only here to check the way is open and the key is known: say goodbye and let
                // the session process make the connection it keeps.
                conn.send_control(&ControlMsg::Bye);
                tokio::time::sleep(Duration::from_millis(100)).await;
                drop(conn);
                self.refresh_desks().await;
                let settings = self.state.lock().expect("app state").settings.clone();
                match (self.launch)(&target, view_flags(&settings, view_only, mute)) {
                    Ok(child) => {
                        if let Some(child) = child {
                            self.watch_session(child, address.clone(), shown.clone());
                        }
                        Dial::Idle
                    }
                    Err(e) => failed(format!("The session window could not be opened: {e:#}")),
                }
            }
            Err(e) => failed(no_answer(&e, &shown).unwrap_or_else(|| describe(&e))),
        };
        self.update(|s| s.dial = outcome);
    }

    /// Closing the app: every connection ends, and on an installed desk the service stops,
    /// so nothing of Illumination is left running and this desk cannot be reached until the
    /// app is opened again.
    fn quit(self: &Arc<Self>) {
        tracing::info!(target: "app", "Closing: ending every connection.");
        for (_, child) in self.opened.lock().expect("opened sessions").drain(..) {
            let _ = child.lock().expect("session window").kill();
        }
        let names = self.state.lock().expect("app state").sessions.clone();
        match &self.hosting.control {
            Some(pipe) => {
                // Told goodbye first, so the desks do not take it for a dropped link.
                for name in names {
                    let _ =
                        crate::control::ask(pipe, &crate::control::Request::EndSession { name });
                }
                std::thread::sleep(Duration::from_millis(300));
                crate::installed::stop_for_app();
            }
            None => {
                for name in names {
                    self.sessions.end(&name);
                }
                // A moment for the goodbyes to leave before the process does.
                std::thread::sleep(Duration::from_millis(300));
            }
        }
    }

    /// Asks the session window open to `address` to come to the front. False when there is
    /// none (or it is on its way out), and then the desk is dialled as usual.
    fn raise(&self, address: &str) -> bool {
        use std::io::Write;
        let opened = self.opened.lock().expect("opened sessions");
        opened
            .iter()
            .filter(|(to, _)| to == address)
            .any(|(_, child)| {
                let mut child = child.lock().expect("session window");
                matches!(child.try_wait(), Ok(None))
                    && child
                        .stdin
                        .as_mut()
                        .is_some_and(|input| input.write_all(b"raise\n").is_ok())
            })
    }

    /// Says so in the window if a session ends with an error within its first moments (it could
    /// not open its window, or the host refused it after all). Its own log has the details.
    fn watch_session(self: &Arc<Self>, child: std::process::Child, address: String, shown: String) {
        let this = self.clone();
        let started = std::time::Instant::now();
        let log = crate::own_folder(&self.home)
            .join("logs")
            .join("viewer.log");
        // Kept where closing the app finds it, and looked at instead of waited on, so that
        // closing can end it.
        let child = Arc::new(Mutex::new(child));
        {
            let mut opened = self.opened.lock().expect("opened sessions");
            opened.push((address, child.clone()));
            let count = opened.len();
            self.update(|s| s.outgoing = count);
        }
        self.runtime.spawn_blocking(move || {
            let status = loop {
                match child.lock().expect("session window").try_wait() {
                    Ok(None) => {}
                    Ok(Some(status)) => break Ok(status),
                    Err(e) => break Err(e),
                }
                std::thread::sleep(Duration::from_millis(200));
            };
            {
                let mut opened = this.opened.lock().expect("opened sessions");
                opened.retain(|(_, other)| !Arc::ptr_eq(other, &child));
                let count = opened.len();
                this.update(|s| s.outgoing = count);
            }
            let early = started.elapsed() < Duration::from_secs(20);
            if early && !status.as_ref().is_ok_and(|s| s.success()) {
                this.update(|s| {
                    if s.dial == Dial::Idle {
                        s.dial = Dial::Failed {
                            target: shown,
                            message: format!(
                                "The session ended right away. Its log says why: {}",
                                log.display()
                            ),
                        };
                    }
                });
            }
        });
    }
}

/// The desks a trust store knows, one entry each, by key: its entry if it has one, and what
/// the older lists (hosts by address, paired peers) say about it.
fn desk_list(store: &illum_core::store::TrustStore) -> Vec<Desk> {
    let mut keys: Vec<&String> = store.desks().map(|(key, _)| key).collect();
    keys.extend(store.paired_peers().map(|(key, _)| key));
    let hosts: Vec<_> = store.known_hosts().collect();
    keys.extend(hosts.iter().map(|(_, host)| &host.fingerprint));
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .map(|key| {
            let info = store.desk(key).cloned().unwrap_or_default();
            let host = hosts.iter().find(|(_, host)| host.fingerprint == *key);
            let peer = store.paired_peer(key);
            let machine = [
                info.machine.as_str(),
                host.map_or("", |(_, h)| h.name.as_str()),
                peer.map_or("", |p| p.name.as_str()),
            ]
            .into_iter()
            .find(|name| !name.is_empty())
            .unwrap_or("unnamed desk")
            .to_string();
            let without_port = |address: &str| {
                address
                    .parse::<std::net::SocketAddr>()
                    .map_or_else(|_| address.to_string(), |a| a.ip().to_string())
            };
            let address = if !info.address.is_empty() {
                info.address.clone()
            } else if let Some((address, _)) = host {
                without_port(address)
            } else {
                peer.map(|p| without_port(&p.last_address))
                    .unwrap_or_default()
            };
            Desk {
                fingerprint: key.clone(),
                name: if info.name.is_empty() {
                    machine
                } else {
                    info.name.clone()
                },
                own_name: info.name.clone(),
                os: info.os.clone(),
                address,
                connect_to: host.map(|(address, _)| (*address).clone()),
                permission: store.permission(key),
                favorite: info.favorite,
                // A host met before desks had entries counts from when it was first met.
                last_out: if info.last_out > 0 {
                    info.last_out
                } else {
                    host.map_or(0, |(_, h)| h.first_seen)
                },
                last_in: if info.last_in > 0 {
                    info.last_in
                } else {
                    peer.map_or(0, |p| p.first_seen)
                },
                tried_in: info.tried_in,
            }
        })
        .collect()
}

/// The plain words for a desk that did not answer at all: off, asleep, not accepting, or
/// not there. `None` for every other failure, which has words of its own.
fn no_answer(err: &illum_net::NetError, shown: &str) -> Option<String> {
    use illum_net::NetError;
    matches!(
        err,
        NetError::Unreachable { .. } | NetError::Timeout("TCP connect" | "name lookup")
    )
    .then(|| format!("No answer from {shown}. It may be off, or not accepting connections."))
}

/// Opens the app window and runs until it is closed.
pub fn run_app(
    runtime: &tokio::runtime::Runtime,
    home: &Path,
    home_flag: Option<PathBuf>,
    in_tray: bool,
) -> Result<()> {
    #[cfg(windows)]
    {
        // Physical pixels for the host side, as `--service` has them.
        illum_win::make_dpi_aware();
        illum_win::leave_own_console();
    }
    let window = AppWindow::new().map_err(anyhow::Error::msg)?;
    let window_waker = window.waker();
    let usual = crate::is_usual_home(home);
    #[cfg(windows)]
    if usual {
        // One app per person: opened again, the running one shows its window.
        let again = window.waker();
        if !illum_win::only_app(move || again.show()) {
            return Ok(());
        }
    }
    if usual {
        // An installed desk's service was stopped when the app was closed: opening the app
        // starts it again. And the app starts with the sign-in, or not, as Settings say.
        crate::installed::start_for_app();
        crate::installed::set_autostart(load_settings(home).start_on_boot);
    }
    let waker: Wake = Arc::new(move |attention| {
        if attention {
            window_waker.attention();
        } else {
            window_waker.wake();
        }
    });
    let backend = Backend::new(
        home,
        endpoint(home)?,
        waker,
        runtime.handle().clone(),
        Hosting {
            listen_on: Vec::new(),
            port: DEFAULT_PORT,
            test_pattern: false,
            service: usual,
            control: crate::installed::control_pipe(home),
        },
        session_process(home_flag),
    );
    backend.start();
    tracing::info!(target: "app", "Illumination {} app window.", env!("CARGO_PKG_VERSION"));
    let state = backend.state.clone();
    window
        .run(state, move |command| backend.command(command), in_tray)
        .map_err(anyhow::Error::msg)
}

/// The window of `illum ask`: one question about a new desk, for the person at this desk. The
/// installed host starts it and reads the answer from how it ends.
pub fn run_ask(name: &str, address: &str, fingerprint: &str, os: &str) -> std::process::ExitCode {
    use crate::installed::{ASK_ACCEPTED, ASK_DECLINED, ASK_NEVER, ASK_ONCE};
    #[cfg(windows)]
    illum_win::leave_own_console();
    let declined = std::process::ExitCode::from(ASK_DECLINED);
    let Ok(window) = AppWindow::new() else {
        return declined;
    };
    let mut state = AppState::default();
    state.questions.push_back(Question {
        id: 1,
        name: name.to_string(),
        address: address.to_string(),
        fingerprint: fingerprint.to_string(),
        os: os.to_string(),
    });
    let answered = window.run_question(Arc::new(Mutex::new(state)), |command| {
        if let Command::Answer { verdict, .. } = command {
            // The answer is the exit code; nothing else is left to do here.
            std::process::exit(i32::from(match verdict {
                Verdict::No => ASK_DECLINED,
                Verdict::Once => ASK_ONCE,
                Verdict::Always => ASK_ACCEPTED,
                Verdict::Never => ASK_NEVER,
            }));
        }
    });
    if let Err(why) = answered {
        tracing::warn!(target: "app", "The question could not be shown: {why}");
    }
    // Closed without an answer.
    declined
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_become_view_flags() {
        let flags = view_flags(&Settings::default(), false, false);
        assert_eq!(
            flags.join(" "),
            "--max-bitrate 20 --max-fps 30 --latency-limit 50 --tab-opacity 30"
        );
        let all = Settings {
            max_bitrate_mbps: 7.5,
            allow_scale: true,
            reconnect: false,
            sound: false,
            clipboard: false,
            ..Settings::default()
        };
        let flags = view_flags(&all, true, false).join(" ");
        for expected in [
            "--max-bitrate 7.5",
            "--allow-scale",
            "--no-reconnect",
            "--mute",
            "--no-clipboard",
            "--view-only",
        ] {
            assert!(flags.contains(expected), "{expected} missing from {flags}");
        }
        assert!(
            view_flags(&Settings::default(), false, true).contains(&"--mute".to_string()),
            "muting one connection"
        );
    }

    #[test]
    fn settings_survive_a_save_and_a_damaged_file() {
        let home = std::env::temp_dir().join(format!("illum-app-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        assert_eq!(load_settings(&home), Settings::default(), "no file yet");
        let chosen = Settings {
            max_fps: 60,
            new_desks: illum_ui::app::Permission::Always,
            ..Settings::default()
        };
        save_settings(&home, &chosen).unwrap();
        assert_eq!(load_settings(&home), chosen);
        // A file from 0.14.5 or before, which only knew "refuse new desks".
        std::fs::write(settings_path(&home), r#"{"refuse_new":true,"max_fps":60}"#).unwrap();
        let old = load_settings(&home);
        assert_eq!(old.new_desks, illum_ui::app::Permission::Never);
        assert_eq!(old.max_fps, 60);
        std::fs::write(settings_path(&home), "not json").unwrap();
        assert_eq!(load_settings(&home), Settings::default());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// What the app starts a session with is a command line this program accepts, with every
    /// setting switched away from its default and a home folder with a space in it.
    #[test]
    fn the_session_command_line_parses() {
        use clap::Parser;
        let all = Settings {
            max_bitrate_mbps: 7.5,
            max_fps: 120,
            latency_limit_ms: 80,
            allow_scale: true,
            reconnect: false,
            sound: false,
            clipboard: false,
            exit_tab_transparency: 100,
            start_on_boot: false,
            let_control: false,
            let_sound: false,
            let_clipboard: false,
            new_desks: illum_ui::app::Permission::Never,
            idle_minutes: 10,
        };
        let home = Path::new("C:\\Program Data\\Illumination");
        for (settings, home) in [(Settings::default(), None), (all, Some(home))] {
            let mut line: Vec<std::ffi::OsString> = vec!["illum".into()];
            line.extend(session_args(
                home,
                "100.64.0.20",
                view_flags(&settings, true, true),
            ));
            let cli = crate::Cli::try_parse_from(&line)
                .unwrap_or_else(|e| panic!("{line:?} is refused: {e}"));
            assert!(matches!(cli.command, Some(crate::Command::View { .. })));
            assert_eq!(cli.home.as_deref(), home);
        }
    }

    type Launches = Arc<Mutex<Vec<(String, Vec<String>)>>>;

    /// One desk's app, without a window: its own key and trust store in a fresh folder,
    /// listening (if started) on a free loopback port and showing the test pattern. Sessions are
    /// noted instead of opened.
    fn desk(name: &str, runtime: &tokio::runtime::Runtime) -> (Arc<Backend>, u16, Launches) {
        let home = std::env::temp_dir().join(format!("illum-app-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        // The listener takes the same number on TCP and on UDP, so the port must be free on
        // both. Asking the system for a free one is not enough: Windows keeps blocks of about a
        // hundred numbers back per protocol (error 10013 on bind) and hands out, for one
        // protocol, numbers inside a block kept from the other. That failed this test one run
        // in six (2026-11-13). So: numbers from below the range the system hands out, spread
        // out, each tried on both.
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let port = (0..200)
            .find_map(|_| {
                let n = NEXT.fetch_add(1, Ordering::Relaxed) * 7919 + u64::from(std::process::id());
                let port = 20_000 + (n % 20_000) as u16;
                std::net::UdpSocket::bind(("127.0.0.1", port)).ok()?;
                std::net::TcpListener::bind(("127.0.0.1", port)).ok()?;
                Some(port)
            })
            .expect("a port free on TCP and UDP");
        let ep = Endpoint {
            identity: Arc::new(illum_net::Identity::generate().unwrap()),
            store: Arc::new(tokio::sync::Mutex::new(
                illum_core::store::TrustStore::load(home.join("trust.json")).unwrap(),
            )),
            device_name: name.into(),
            timeouts: illum_net::Timeouts::default(),
            prompt: Arc::new(|_| Box::pin(async { Answer::No })), // replaced by the window's
            capabilities: illum_net::session::all_capabilities(),
        };
        let launches = Launches::default();
        let noted = launches.clone();
        let backend = Backend::new(
            &home,
            ep,
            Arc::new(|_| {}),
            runtime.handle().clone(),
            Hosting {
                listen_on: vec!["127.0.0.1".parse().unwrap()],
                port,
                test_pattern: true,
                service: false,
                control: None,
            },
            Arc::new(move |target, flags| {
                noted.lock().unwrap().push((target.to_string(), flags));
                Ok(None)
            }),
        );
        (backend, port, launches)
    }

    /// Waits until the desk's state satisfies `ready` and returns what it took out of it.
    fn wait<T>(what: &str, desk: &Backend, ready: impl Fn(&AppState) -> Option<T>) -> T {
        let until = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(found) = ready(&desk.state.lock().unwrap()) {
                return found;
            }
            assert!(std::time::Instant::now() < until, "timed out: {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn connect_to(desk: &Arc<Backend>, target: &str) {
        desk.command(Command::Connect {
            target: target.to_string(),
            view_only: true,
            mute: false,
        });
    }

    /// The whole of what the window drives, between two desks on loopback: a first connection
    /// with both key questions, the lists that result, a second connection without questions,
    /// an address nobody listens on, and a desk that is forgotten and then declined.
    #[test]
    fn two_desks_connect_through_the_app() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let (host, port, _) = desk("host", &runtime);
        let (viewer, _, launches) = desk("viewer", &runtime);
        host.start();
        viewer.start();
        wait("the host listens", &host, |s| match &s.incoming {
            Incoming::Listening => Some(()),
            Incoming::Failed(why) => panic!("the host cannot listen: {why}"),
            Incoming::Off => None,
        });
        let target = format!("127.0.0.1:{port}");

        // First connection: the viewer is asked nothing (the host's key is remembered as it
        // is met); the host is asked whether to let the viewer in.
        connect_to(&viewer, &target);
        let asked = wait("the host is asked to accept the viewer", &host, |s| {
            s.questions.front().cloned()
        });
        assert_eq!(asked.name, "viewer");
        assert_eq!(asked.fingerprint, viewer.state.lock().unwrap().fingerprint);
        host.command(Command::Answer {
            id: asked.id,
            verdict: Verdict::Always,
        });
        wait("the session is opened", &viewer, |s| {
            (s.dial == Dial::Idle && !launches.lock().unwrap().is_empty()).then_some(())
        });
        let (opened, flags) = launches.lock().unwrap()[0].clone();
        assert_eq!(opened, target);
        assert!(flags.contains(&"--view-only".to_string()), "{flags:?}");
        let desks = wait("the host is among the viewer's desks", &viewer, |s| {
            (!s.desks.is_empty()).then(|| s.desks.clone())
        });
        wait("the host is marked online on the viewer", &viewer, |s| {
            (s.online.get(&target) == Some(&true)).then_some(())
        });
        assert_eq!(
            (desks[0].name.as_str(), desks[0].connect_to.as_deref()),
            ("host", Some(target.as_str()))
        );
        assert_eq!(desks[0].address, "127.0.0.1", "shown without its port");
        assert!(!desks[0].os.is_empty() && desks[0].last_out > 0);
        let trusted = wait("the viewer is among the host's desks, let in", &host, |s| {
            s.desks
                .iter()
                .find(|d| d.permission == Some(illum_ui::app::Permission::Always))
                .cloned()
        });
        assert_eq!(trusted.name, "viewer");
        assert!(
            trusted.connect_to.is_none(),
            "it came here; the host never dialled it"
        );
        assert!(trusted.last_in > 0);

        // The person's own name, a star and a permission for one desk.
        host.command(Command::EditDesk {
            fingerprint: trusted.fingerprint.clone(),
            name: "My viewer".into(),
            permission: Some(illum_ui::app::Permission::Always),
            favorite: true,
        });
        wait("the desk shows as edited", &host, |s| {
            s.desks
                .iter()
                .any(|d| d.name == "My viewer" && d.favorite)
                .then_some(())
        });
        wait("no question is left on either screen", &host, |s| {
            s.questions.is_empty().then_some(())
        });
        assert!(viewer.state.lock().unwrap().questions.is_empty());
        wait("the check connection is gone from the host", &host, |s| {
            s.sessions.is_empty().then_some(())
        });

        // Again: both know each other, nobody is asked.
        connect_to(&viewer, &target);
        wait("the second session is opened", &viewer, |s| {
            (s.dial == Dial::Idle && launches.lock().unwrap().len() == 2).then_some(())
        });
        assert!(host.state.lock().unwrap().questions.is_empty());

        // Nobody listens there: said in the window, and dismissed.
        let nowhere = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        connect_to(&viewer, &nowhere.to_string());
        let message = wait("the failure is shown", &viewer, |s| match &s.dial {
            Dial::Failed { message, .. } => Some(message.clone()),
            _ => None,
        });
        assert!(message.starts_with("No answer from 127.0.0.1"), "{message}");
        viewer.command(Command::DismissDial);
        // A name is not an address.
        connect_to(&viewer, "laptop");
        let message = wait("a name is turned down", &viewer, |s| match &s.dial {
            Dial::Failed { message, .. } => Some(message.clone()),
            _ => None,
        });
        assert!(message.contains("IP address"), "{message}");
        viewer.command(Command::DismissDial);
        assert_eq!(viewer.state.lock().unwrap().dial, Dial::Idle);
        assert_eq!(
            launches.lock().unwrap().len(),
            2,
            "nothing opened for a failure"
        );

        // The host forgets the viewer, is asked again, and declines.
        host.command(Command::ForgetDesk(trusted.fingerprint.clone()));
        wait("the host forgot the viewer", &host, |s| {
            s.desks.is_empty().then_some(())
        });
        connect_to(&viewer, &target);
        let asked = wait("the host is asked again", &host, |s| {
            s.questions.front().cloned()
        });
        host.command(Command::Answer {
            id: asked.id,
            verdict: Verdict::No,
        });
        wait("the refusal is shown to the viewer", &viewer, |s| {
            matches!(s.dial, Dial::Failed { .. }).then_some(())
        });
        assert_eq!(launches.lock().unwrap().len(), 2);

        // The viewer forgets the host.
        viewer.command(Command::ForgetDesk(desks[0].fingerprint.clone()));
        wait("the viewer forgot the host", &viewer, |s| {
            s.desks.is_empty().then_some(())
        });

        // A setting is saved as it is changed.
        host.command(Command::SaveSettings(Settings {
            new_desks: illum_ui::app::Permission::Never,
            ..Settings::default()
        }));
        assert_eq!(
            load_settings(&host.home).new_desks,
            illum_ui::app::Permission::Never,
            "saved"
        );
    }

    /// What the person at a desk decides about a desk that reaches it: in for this time
    /// only, sent away, back without a question while it has just left, and limited in
    /// what it may do.
    #[test]
    fn a_guest_is_let_in_sent_away_and_limited() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let (host, port, _) = desk("guarded", &runtime);
        host.start();
        wait("the host listens", &host, |s| {
            (s.incoming == Incoming::Listening).then_some(())
        });
        let target = format!("127.0.0.1:{port}");
        let guest_home =
            std::env::temp_dir().join(format!("illum-app-guest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&guest_home);
        let guest = Endpoint {
            identity: Arc::new(illum_net::Identity::generate().unwrap()),
            store: Arc::new(tokio::sync::Mutex::new(
                illum_core::store::TrustStore::load(guest_home.join("trust.json")).unwrap(),
            )),
            device_name: "guest".into(),
            timeouts: illum_net::Timeouts::default(),
            prompt: Arc::new(|_| Box::pin(async { Answer::Always })),
            capabilities: illum_net::session::all_capabilities(),
        };
        let dial = || {
            let (guest, target) = (guest.clone(), target.clone());
            runtime.spawn(async move { connect(&guest, &target).await })
        };
        let gone = |host: &Arc<Backend>| {
            wait("nobody is connected", host, |s| {
                s.sessions.is_empty().then_some(())
            })
        };
        let this_time = |host: &Arc<Backend>| {
            let asked = wait("the host is asked", host, |s| s.questions.front().cloned());
            host.command(Command::Answer {
                id: asked.id,
                verdict: Verdict::Once,
            });
        };

        // Accepted for this time: connected, and not among the trusted desks.
        let dialing = dial();
        this_time(&host);
        let mut conn = runtime.block_on(dialing).unwrap().unwrap();
        let offered = conn.peer_hello.clone().unwrap().capabilities;
        assert!(offered.iter().any(|c| c == "audio"), "{offered:?}");
        wait("the guest is shown as connected", &host, |s| {
            (s.sessions == ["guest"]).then_some(())
        });
        assert!(
            host.state
                .lock()
                .unwrap()
                .desks
                .iter()
                .all(|d| d.permission.is_none()),
            "this time only: no permission of its own"
        );

        // Sent away: it is told goodbye, so it does not come back by itself.
        // As in a real session, it is in the middle of sending when that happens: the
        // goodbye must reach it all the same.
        let chatter = conn.sender();
        let chatting = runtime.spawn(async move {
            for seq in 0.. {
                if chatter.is_closed() {
                    break;
                }
                chatter.send_control(&ControlMsg::Ping { seq });
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });
        host.command(Command::EndSession("guest".into()));
        let said = runtime.block_on(async {
            loop {
                match conn.recv_control(Duration::from_secs(5), "goodbye").await {
                    Ok(ControlMsg::Bye) => break Ok(()),
                    Ok(_) => {}
                    Err(e) => break Err(e),
                }
            }
        });
        assert!(said.is_ok(), "{said:?}");
        chatting.abort();
        drop(conn);
        gone(&host);

        // Sent away means asked about again.
        let dialing = dial();
        this_time(&host);
        let conn = runtime.block_on(dialing).unwrap().unwrap();
        wait("the guest is connected again", &host, |s| {
            (s.sessions == ["guest"]).then_some(())
        });
        // It leaves by itself (a dropped link looks the same): back without a question.
        drop(conn);
        gone(&host);
        let conn = runtime.block_on(dial()).unwrap().unwrap();
        assert!(host.state.lock().unwrap().questions.is_empty());
        drop(conn);
        gone(&host);

        // No sound for whoever connects from now on. (The test pattern has a hum and no
        // keyboard to switch off; the three switches take the same way.)
        host.command(Command::SaveSettings(Settings {
            let_sound: false,
            ..Settings::default()
        }));
        let until = std::time::Instant::now() + Duration::from_secs(20);
        let staying = loop {
            // The host listens anew; for a moment nobody is there.
            if let Ok(Ok(conn)) = runtime.block_on(dial()) {
                let offered = conn.peer_hello.clone().unwrap().capabilities;
                if !offered.iter().any(|c| c == "audio") {
                    assert!(offered.iter().any(|c| c == "screen"), "{offered:?}");
                    break conn;
                }
            }
            assert!(std::time::Instant::now() < until, "sound is still offered");
            std::thread::sleep(Duration::from_millis(50));
        };

        // While that desk is connected, who may connect stays as it is: it could be the one
        // changing it, through this desk's screen.
        wait("the guest is connected", &host, |s| {
            (s.sessions == ["guest"]).then_some(())
        });
        host.command(Command::SaveSettings(Settings::default()));
        assert!(
            !host.state.lock().unwrap().settings.let_sound,
            "changed while a desk is connected"
        );
        // Once it has gone the change is made, and the port, held a moment ago, is listened
        // on again.
        drop(staying);
        gone(&host);
        host.command(Command::SaveSettings(Settings::default()));
        let until = std::time::Instant::now() + Duration::from_secs(20);
        let second = loop {
            if let Ok(Ok(conn)) = runtime.block_on(dial()) {
                let offered = conn.peer_hello.clone().unwrap().capabilities;
                if offered.iter().any(|c| c == "audio") {
                    break conn;
                }
            }
            assert!(
                std::time::Instant::now() < until,
                "sound is not offered again"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        if let Incoming::Failed(why) = &host.state.lock().unwrap().incoming {
            panic!("the host cannot listen again: {why}");
        }
        // Two sessions of one desk (as with a display popped out) are one desk in the list.
        let mut staying = runtime.block_on(dial()).unwrap().unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(host.state.lock().unwrap().sessions, ["guest"]);
        assert_eq!(host.state.lock().unwrap().guests.len(), 1);
        staying.send_control(&ControlMsg::Ping { seq: 7 });
        let answered = runtime.block_on(staying.recv_control(Duration::from_secs(5), "pong"));
        assert!(
            matches!(answered, Ok(ControlMsg::Pong { seq: 7 })),
            "{answered:?}"
        );
        drop(second);
        let _ = std::fs::remove_dir_all(&guest_home);
    }
}
