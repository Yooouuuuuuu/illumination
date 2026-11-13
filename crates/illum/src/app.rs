// SPDX-License-Identifier: GPL-3.0-only
//! `illum` with no command: the app. This is the half behind the window (`illum_ui::app`,
//! architecture §14): it keeps the window's [`AppState`] true and carries out its [`Command`]s.
//!
//! - **This desk** listens in this process while "Allow incoming connections" is on, exactly as
//!   `illum --service` does, with the key questions asked in the window instead of a console.
//! - **Connect** first dials from here, so a wrong address, a refusal or a changed key is said
//!   in the window and a first connection gets its key check there. Then the session opens in
//!   its own process (`illum view …`): one window per session, and a session that crashes does
//!   not take the app or the other sessions with it. AnyDesk and RustDesk do the same.
//! - **Settings** live in `settings.json` in the home folder and become the flags of the next
//!   `illum view`.

use crate::{SessionEvent, accept_loop, describe, endpoint, host_config, open_listener};
use anyhow::{Context, Result};
use illum_core::protocol::{ControlMsg, DEFAULT_PORT};
use illum_net::session::{connect, normalize_target};
use illum_net::{Endpoint, Prompt, PromptKind, PromptRequest, bind};
use illum_ui::app::{
    AppState, Command, Desk, Dial, Incoming, Question, QuestionKind, Settings, TrustedDesk,
};
use illum_ui::app_window::AppWindow;
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

type Shared = Arc<Mutex<AppState>>;
/// Who is waiting for the answer to each question on screen.
type Answers = Arc<Mutex<HashMap<u64, oneshot::Sender<bool>>>>;

fn settings_path(home: &Path) -> PathBuf {
    home.join("settings.json")
}

/// The saved settings; the defaults when there are none or the file cannot be read (a damaged
/// settings file must not keep the app from opening).
fn load_settings(home: &Path) -> Settings {
    let path = settings_path(home);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!(target: "app", "{} is not readable ({e}); using the defaults.", path.display());
            Settings::default()
        }),
        Err(_) => Settings::default(),
    }
}

fn save_settings(home: &Path, settings: &Settings) -> Result<()> {
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
        "--keyboard".to_string(),
        match settings.keyboard {
            illum_ui::KeyboardMode::Remote => "remote",
            illum_ui::KeyboardMode::Local => "local",
            illum_ui::KeyboardMode::Auto => "auto",
        }
        .to_string(),
        "--pointer".to_string(),
        settings.pointer.to_string(),
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
                    kind: match req.kind {
                        PromptKind::TrustHost => QuestionKind::TrustHost,
                        PromptKind::PairPeer => QuestionKind::AcceptDesk,
                    },
                    name: if req.name.is_empty() {
                        "this desk".to_string()
                    } else {
                        req.name.clone()
                    },
                    address: req.address.clone(),
                    fingerprint: req.fingerprint.as_str().to_string(),
                });
            waker(true);
            let _on_screen = Asked {
                id,
                state,
                answers,
                waker,
            };
            asked.await.unwrap_or(false)
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
}

struct Backend {
    home: PathBuf,
    ep: Endpoint,
    state: Shared,
    answers: Answers,
    waker: Wake,
    runtime: tokio::runtime::Handle,
    hosting: Hosting,
    launch: Launch,
    /// The task that listens for other desks, while "Allow incoming connections" is on.
    listening: Mutex<Option<JoinHandle<()>>>,
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
            .stdin(std::process::Stdio::null())
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
        Arc::new(Backend {
            home: home.to_path_buf(),
            ep,
            state,
            answers,
            waker,
            runtime,
            hosting,
            launch,
            listening: Mutex::new(None),
        })
    }

    /// Fills the lists from the trust store and starts listening if the settings say so.
    fn start(self: &Arc<Self>) {
        let this = self.clone();
        self.runtime
            .spawn(async move { this.refresh_desks().await });
        if self
            .state
            .lock()
            .expect("app state")
            .settings
            .allow_incoming
        {
            self.set_incoming(true);
        }
    }

    fn update(&self, change: impl FnOnce(&mut AppState)) {
        change(&mut self.state.lock().expect("app state"));
        (self.waker)(false);
    }

    /// Copies the trust store's two lists into what the window shows.
    async fn refresh_desks(&self) {
        let (desks, trusted) = {
            let store = self.ep.store.lock().await;
            let desks = store
                .known_hosts()
                .map(|(address, host)| Desk {
                    address: address.clone(),
                    name: host.name.clone(),
                    fingerprint: host.fingerprint.clone(),
                    first_seen: host.first_seen,
                })
                .collect();
            let trusted = store
                .paired_peers()
                .map(|(fingerprint, peer)| TrustedDesk {
                    fingerprint: fingerprint.clone(),
                    name: peer.name.clone(),
                    last_address: peer.last_address.clone(),
                })
                .collect();
            (desks, trusted)
        };
        self.update(|s| {
            s.desks = desks;
            s.trusted = trusted;
        });
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
            Command::Answer { id, accept } => {
                if let Some(answer) = self.answers.lock().expect("answers").remove(&id) {
                    let _ = answer.send(accept);
                }
            }
            Command::ForgetDesk(address) => {
                self.runtime.spawn(async move {
                    if let Err(e) = this.ep.store.lock().await.forget_host(&address) {
                        tracing::warn!(target: "app", "Could not forget {address}: {e}");
                    }
                    this.refresh_desks().await;
                });
            }
            Command::ForgetTrusted(fingerprint) => {
                self.runtime.spawn(async move {
                    if let Err(e) = this.ep.store.lock().await.forget_peer(&fingerprint) {
                        tracing::warn!(target: "app", "Could not forget a trusted desk: {e}");
                    }
                    this.refresh_desks().await;
                });
            }
            Command::SaveSettings(settings) => {
                let was = self.state.lock().expect("app state").settings.clone();
                if let Err(e) = save_settings(&self.home, &settings) {
                    tracing::warn!(target: "app", "Settings not saved: {e:#}");
                }
                let incoming = settings.allow_incoming;
                self.update(|s| s.settings = settings);
                if incoming != was.allow_incoming {
                    self.set_incoming(incoming);
                }
            }
        }
    }

    /// Starts or stops listening for other desks.
    fn set_incoming(self: &Arc<Self>, on: bool) {
        let mut listening = self.listening.lock().expect("listening task");
        if let Some(task) = listening.take() {
            task.abort();
        }
        if !on {
            self.update(|s| s.incoming = Incoming::Off);
            return;
        }
        let this = self.clone();
        let test_pattern = self.hosting.test_pattern;
        *listening = Some(self.runtime.spawn(async move {
            // Probing the encoders can take a moment; not on the async threads.
            let config =
                match tokio::task::spawn_blocking(move || host_config(test_pattern, false, false))
                    .await
                {
                    Ok(config) => config,
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
                    let shown = this.clone();
                    let on_session = Arc::new(move |event: SessionEvent| {
                        if matches!(event, SessionEvent::Started(_)) {
                            // It may have been accepted a moment ago: show it as trusted.
                            let lists = shown.clone();
                            shown
                                .runtime
                                .spawn(async move { lists.refresh_desks().await });
                        }
                        shown.update(|s| match event {
                            SessionEvent::Started(name) => s.sessions.push(name),
                            SessionEvent::Ended(name) => {
                                if let Some(i) = s.sessions.iter().position(|n| *n == name) {
                                    s.sessions.remove(i);
                                }
                            }
                        });
                    });
                    accept_loop(ep, listener, config, on_session).await;
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
                             Close it, then switch this off and on.",
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

    /// Reaches `target` from here (key check and errors in the window), then opens the session
    /// in its own process.
    async fn dial(self: Arc<Self>, target: String, view_only: bool, mute: bool) {
        let address = normalize_target(&target);
        let shown = illum_ui::app::shown_address(&address).to_string();
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
                            self.watch_session(child, shown.clone());
                        }
                        Dial::Idle
                    }
                    Err(e) => failed(format!("The session window could not be opened: {e:#}")),
                }
            }
            Err(e) => failed(describe(&e)),
        };
        self.update(|s| s.dial = outcome);
    }

    /// Says so in the window if a session ends with an error within its first moments (it could
    /// not open its window, or the host refused it after all). Its own log has the details.
    fn watch_session(self: &Arc<Self>, mut child: std::process::Child, shown: String) {
        let this = self.clone();
        let started = std::time::Instant::now();
        self.runtime.spawn_blocking(move || {
            let status = child.wait();
            let early = started.elapsed() < Duration::from_secs(20);
            if early && !status.as_ref().is_ok_and(|s| s.success()) {
                this.update(|s| {
                    if s.dial == Dial::Idle {
                        s.dial = Dial::Failed {
                            target: shown,
                            message: "The session ended right away. The log file viewer.log \
                                      in the Illumination folder says why."
                                .to_string(),
                        };
                    }
                });
            }
        });
    }
}

/// Opens the app window and runs until it is closed.
pub fn run_app(
    runtime: &tokio::runtime::Runtime,
    home: &Path,
    home_flag: Option<PathBuf>,
) -> Result<()> {
    #[cfg(windows)]
    {
        // Physical pixels for the host side, as `--service` has them.
        illum_win::make_dpi_aware();
        illum_win::leave_own_console();
    }
    let window = AppWindow::new().map_err(anyhow::Error::msg)?;
    let window_waker = window.waker();
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
        },
        session_process(home_flag),
    );
    backend.start();
    tracing::info!(target: "app", "Illumination {} app window.", env!("CARGO_PKG_VERSION"));
    let state = backend.state.clone();
    window
        .run(state, move |command| backend.command(command))
        .map_err(anyhow::Error::msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_become_view_flags() {
        let flags = view_flags(&Settings::default(), false, false);
        assert_eq!(
            flags.join(" "),
            "--max-bitrate 20 --max-fps 30 --latency-limit 50 --keyboard remote --pointer native"
        );
        let all = Settings {
            max_bitrate_mbps: 7.5,
            allow_scale: true,
            reconnect: false,
            sound: false,
            clipboard: false,
            keyboard: illum_ui::KeyboardMode::Local,
            pointer: illum_ui::PointerMode::Embedded,
            ..Settings::default()
        };
        let flags = view_flags(&all, true, false).join(" ");
        for expected in [
            "--max-bitrate 7.5",
            "--keyboard local",
            "--pointer embedded",
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
            allow_incoming: false,
            ..Settings::default()
        };
        save_settings(&home, &chosen).unwrap();
        assert_eq!(load_settings(&home), chosen);
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
            keyboard: illum_ui::KeyboardMode::Auto,
            pointer: illum_ui::PointerMode::Embedded,
            allow_incoming: true,
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
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let ep = Endpoint {
            identity: Arc::new(illum_net::Identity::generate().unwrap()),
            store: Arc::new(tokio::sync::Mutex::new(
                illum_core::store::TrustStore::load(home.join("trust.json")).unwrap(),
            )),
            device_name: name.into(),
            timeouts: illum_net::Timeouts::default(),
            prompt: Arc::new(|_| Box::pin(async { false })), // replaced by the window's
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
        wait("the host listens", &host, |s| {
            (s.incoming == Incoming::Listening).then_some(())
        });
        let target = format!("127.0.0.1:{port}");

        // First connection: the viewer checks the host's key, the host accepts the viewer.
        connect_to(&viewer, &target);
        let asked = wait("the viewer is asked about the host's key", &viewer, |s| {
            s.questions.front().cloned()
        });
        assert_eq!(asked.kind, QuestionKind::TrustHost);
        assert_eq!(asked.fingerprint, host.state.lock().unwrap().fingerprint);
        assert!(matches!(
            viewer.state.lock().unwrap().dial,
            Dial::Dialing(_)
        ));
        viewer.command(Command::Answer {
            id: asked.id,
            accept: true,
        });
        let asked = wait("the host is asked to accept the viewer", &host, |s| {
            s.questions.front().cloned()
        });
        assert_eq!(asked.kind, QuestionKind::AcceptDesk);
        assert_eq!(asked.name, "viewer");
        assert_eq!(asked.fingerprint, viewer.state.lock().unwrap().fingerprint);
        host.command(Command::Answer {
            id: asked.id,
            accept: true,
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
        assert_eq!(
            (desks[0].name.as_str(), desks[0].address.as_str()),
            ("host", target.as_str())
        );
        let trusted = wait("the viewer is among the host's trusted desks", &host, |s| {
            (!s.trusted.is_empty()).then(|| s.trusted.clone())
        });
        assert_eq!(trusted[0].name, "viewer");
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
        assert!(!message.is_empty());
        viewer.command(Command::DismissDial);
        assert_eq!(viewer.state.lock().unwrap().dial, Dial::Idle);
        assert_eq!(
            launches.lock().unwrap().len(),
            2,
            "nothing opened for a failure"
        );

        // The host forgets the viewer, is asked again, and declines.
        host.command(Command::ForgetTrusted(trusted[0].fingerprint.clone()));
        wait("the host forgot the viewer", &host, |s| {
            s.trusted.is_empty().then_some(())
        });
        connect_to(&viewer, &target);
        let asked = wait("the host is asked again", &host, |s| {
            s.questions.front().cloned()
        });
        host.command(Command::Answer {
            id: asked.id,
            accept: false,
        });
        wait("the refusal is shown to the viewer", &viewer, |s| {
            matches!(s.dial, Dial::Failed { .. }).then_some(())
        });
        assert_eq!(launches.lock().unwrap().len(), 2);

        // The viewer forgets the host.
        viewer.command(Command::ForgetDesk(target.clone()));
        wait("the viewer forgot the host", &viewer, |s| {
            s.desks.is_empty().then_some(())
        });

        // Turning incoming off stops the listening.
        host.command(Command::SaveSettings(Settings {
            allow_incoming: false,
            ..Settings::default()
        }));
        assert_eq!(host.state.lock().unwrap().incoming, Incoming::Off);
        assert!(!load_settings(&host.home).allow_incoming, "saved");
    }
}
