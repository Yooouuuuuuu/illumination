// SPDX-License-Identifier: GPL-3.0-only
//! What the app asks of the installed host (architecture §3).
//!
//! On an installed desk the home folder belongs to the system: the person's programs read
//! it and may not write it, or any account on the PC could add a desk of its own to the
//! trusted ones. So the app asks the host to make its changes, over a local pipe that only
//! the system and the person signed in at the screen can open: one request, one answer, a
//! line of JSON each.

use illum_core::store::{Change, Through, TrustStore};
use illum_ui::app::Settings;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "ask", rename_all = "snake_case")]
pub enum Request {
    /// A change to the desks this one knows.
    Trust { change: Change },
    /// The settings as the person left them.
    Settings { settings: Settings },
    /// End the sessions of the desk of this name that is connected to this one.
    EndSession { name: String },
    /// Update to the latest release: download it, check it and run its installer.
    Update,
    /// Stop a download on its way.
    CancelUpdate,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reply {
    pub ok: bool,
    /// Whether anything changed.
    pub changed: bool,
    /// Why not, when `ok` is false.
    pub why: String,
}

/// The pipe of the host that serves `home`. The usual home has the usual name; any other
/// (tests, a second desk on one PC) gets a name of its own.
pub fn pipe_name(home: &Path, usual: bool) -> String {
    if usual {
        return r"\\.\pipe\Illumination".to_string();
    }
    // FNV-1a over the path: stable, and enough to tell folders apart.
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in home.to_string_lossy().to_lowercase().bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
    }
    format!(r"\\.\pipe\Illumination-{hash:016x}")
}

/// Ends the sessions of the desk of that name; says how many there were.
pub type EndSessions = Arc<dyn Fn(&str) -> usize + Send + Sync>;

/// What the host does for the app besides changing files.
#[derive(Clone)]
pub struct Hands {
    pub end: EndSessions,
    /// The updater; `None` where there is none (tests).
    pub update: Option<Arc<crate::update::Installer>>,
}

/// Carries out one request in the home folder. This is the host's side.
pub fn carry_out(home: &Path, hands: &Hands, request: &Request) -> Reply {
    let done = match request {
        Request::Trust { change } => TrustStore::load(home.join("trust.json"))
            .and_then(|mut store| store.apply(change))
            .map_err(|e| e.to_string()),
        Request::Settings { settings } => crate::app::save_settings(home, settings)
            .map(|()| true)
            .map_err(|e| format!("{e:#}")),
        Request::EndSession { name } => Ok((hands.end)(name) > 0),
        Request::Update => match &hands.update {
            Some(installer) => installer.start().map(|()| true),
            None => Err("this host does not update".to_string()),
        },
        Request::CancelUpdate => {
            if let Some(installer) = &hands.update {
                installer.cancel();
            }
            Ok(true)
        }
    };
    match done {
        Ok(changed) => Reply {
            ok: true,
            changed,
            why: String::new(),
        },
        Err(why) => Reply {
            ok: false,
            changed: false,
            why,
        },
    }
}

/// Answers one line.
fn answer(home: &Path, hands: &Hands, line: &str) -> Reply {
    let request = match serde_json::from_str::<Request>(line) {
        Ok(request) => request,
        Err(e) => {
            return Reply {
                ok: false,
                changed: false,
                why: format!("not a request this version knows: {e}"),
            };
        }
    };
    let what = match &request {
        Request::Trust {
            change: Change::PinHost { address, .. },
        } => format!("remember the desk at {address}"),
        Request::Trust {
            change: Change::ForgetHost { address },
        } => format!("forget the desk at {address}"),
        Request::Trust {
            change: Change::ForgetPeer { .. },
        } => "forget a trusted desk".to_string(),
        Request::Trust {
            change: Change::ConnectedTo { address, .. },
        } => format!("note a connection to {address}"),
        Request::Trust { change } => match change {
            Change::SetPermission { permission, .. } => {
                format!("set a desk's permission to {permission:?}")
            }
            Change::Rename { .. } => "rename a desk".to_string(),
            Change::Favorite { .. } => "star a desk".to_string(),
            _ => "forget a desk".to_string(),
        },
        Request::Settings { .. } => "save the settings".to_string(),
        Request::EndSession { name } => format!("disconnect \"{name}\""),
        Request::Update => "update to the latest release".to_string(),
        Request::CancelUpdate => "stop the update".to_string(),
    };
    tracing::info!(target: "service", "Asked by the app: {what}.");
    let reply = carry_out(home, hands, &request);
    if !reply.ok {
        tracing::warn!(target: "service", "Not done: {}", reply.why);
    }
    reply
}

/// Asks the host at `pipe`. `Ok(changed)` when it did it.
pub fn ask(pipe: &str, request: &Request) -> Result<bool, String> {
    use std::io::{BufRead, BufReader, Write};
    let mut line = serde_json::to_string(request).map_err(|e| e.to_string())?;
    line.push('\n');
    // The host serves one program at a time and opens the pipe anew after each: a moment
    // of "busy" or "not there" is normal.
    let until = std::time::Instant::now() + Duration::from_secs(3);
    let mut pipe_file = loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(pipe)
        {
            Ok(file) => break file,
            Err(_) if std::time::Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("the Illumination service does not answer ({e})")),
        }
    };
    pipe_file
        .write_all(line.as_bytes())
        .map_err(|e| format!("asking the Illumination service: {e}"))?;
    let mut answer = String::new();
    BufReader::new(&pipe_file)
        .read_line(&mut answer)
        .map_err(|e| format!("reading the Illumination service's answer: {e}"))?;
    let reply: Reply = serde_json::from_str(&answer)
        .map_err(|e| format!("the Illumination service answered something else ({e})"))?;
    if reply.ok {
        Ok(reply.changed)
    } else {
        Err(reply.why)
    }
}

/// A trust store's changes, asked of the host at `pipe`.
pub fn trust_through(pipe: String) -> Through {
    Arc::new(move |change: &Change| {
        ask(
            &pipe,
            &Request::Trust {
                change: change.clone(),
            },
        )
    })
}

/// Serves the pipe until the task is dropped: one program at a time, each asked for one
/// line and given one. Opened for the system and for whoever is signed in at this screen
/// now; when run by hand (not as the system), for the person running it.
#[cfg(windows)]
pub async fn serve(home: std::path::PathBuf, pipe: String, hands: Hands) {
    if illum_win::runs_as_system() {
        let signed_in = || illum_win::own_session().and_then(illum_win::session_user_sid);
        serve_for(home, pipe, hands, Some(signed_in)).await
    } else {
        serve_for(home, pipe, hands, None::<fn() -> Option<String>>).await
    }
}

/// [`serve`], for the system and the account `guest` names at the time (`None`: for
/// whoever runs this, as Windows does by itself).
#[cfg(windows)]
async fn serve_for(
    home: std::path::PathBuf,
    pipe: String,
    hands: Hands,
    guest: Option<impl Fn() -> Option<String>>,
) {
    use std::os::windows::io::AsRawHandle;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::windows::named_pipe::ServerOptions;

    /// How long an unused pipe stands before it is opened anew, for whoever is signed in
    /// by then.
    const STANDS: Duration = Duration::from_secs(5);
    /// How long a program has to say its line and take the answer.
    const PATIENCE: Duration = Duration::from_secs(3);
    /// A request is small; anything longer is not one.
    const LONGEST_REQUEST: u64 = 64 * 1024;
    // Right after a request the pipe just served is still closing, and opening the next
    // one is refused for a moment: tried again at once, and only a lasting refusal is said.
    let mut refused = 0u32;
    loop {
        let opened = {
            let mut options = ServerOptions::new();
            // Refused if a pipe of this name exists already: nobody gets in front of us.
            options.first_pipe_instance(true).max_instances(1);
            if let Some(guest) = &guest {
                match illum_win::PipeSecurity::system_and(guest().as_deref()) {
                    // Windows copies the list as it makes the pipe.
                    Some(mut security) => unsafe {
                        options.create_with_security_attributes_raw(&pipe, security.as_ptr())
                    },
                    None => Err(std::io::Error::other("no access list")),
                }
            } else {
                options.create(&pipe)
            }
        };
        let server = match opened {
            Ok(server) => server,
            Err(e) => {
                refused += 1;
                if refused == 40 {
                    tracing::warn!(target: "service", "The app cannot reach this host: opening {pipe} failed: {e}");
                }
                tokio::time::sleep(if refused < 40 {
                    Duration::from_millis(25)
                } else {
                    STANDS
                })
                .await;
                continue;
            }
        };
        refused = 0;
        if !matches!(
            tokio::time::timeout(STANDS, server.connect()).await,
            Ok(Ok(()))
        ) {
            continue;
        }
        // A program from another session has no business here, whoever it runs as.
        let theirs = illum_win::pipe_client_session(server.as_raw_handle().cast());
        if guest.is_some() && theirs != illum_win::own_session() {
            tracing::warn!(target: "service", "A program in session {theirs:?} asked; not answered.");
            continue;
        }
        let home = home.clone();
        let hands = hands.clone();
        let served = async move {
            let (reading, mut writing) = tokio::io::split(server);
            let mut reading = BufReader::new(reading);
            let mut line = String::new();
            (&mut reading)
                .take(LONGEST_REQUEST)
                .read_line(&mut line)
                .await?;
            let reply = tokio::task::spawn_blocking(move || answer(&home, &hands, line.trim()))
                .await
                .map_err(std::io::Error::other)?;
            let mut text = serde_json::to_string(&reply).map_err(std::io::Error::other)?;
            text.push('\n');
            writing.write_all(text.as_bytes()).await?;
            writing.flush().await?;
            // Until the program has taken the answer and let go; closing first could lose it.
            let mut rest = [0u8; 1];
            let _ = reading.read(&mut rest).await;
            std::io::Result::Ok(())
        };
        match tokio::time::timeout(PATIENCE, served).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::debug!(target: "service", "A request ended early: {e}"),
            Err(_) => {
                tracing::debug!(target: "service", "A program opened the pipe and did not finish.")
            }
        }
    }
}

#[cfg(not(windows))]
pub async fn serve(_home: std::path::PathBuf, _pipe: String, _hands: Hands) {
    // No installed service on the other systems yet (architecture §3).
    std::future::pending::<()>().await
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// A host serving a folder, and a program asking it: the changes land in the folder,
    /// nonsense is refused, and many requests in a row all get through the one pipe.
    #[test]
    fn the_app_asks_and_the_host_does() {
        let home = std::env::temp_dir().join(format!("illum-control-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let pipe = pipe_name(&home, false);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let ended = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let noted = ended.clone();
        let end: EndSessions = Arc::new(move |name: &str| {
            noted.lock().unwrap().push(name.to_string());
            usize::from(name == "laptop")
        });
        let hands = Hands { end, update: None };
        let serving = runtime.spawn(serve(home.clone(), pipe.clone(), hands));

        let mut store = TrustStore::load(home.join("trust.json")).unwrap();
        store.write_through(trust_through(pipe.clone()));
        for n in 0..20 {
            store
                .pin_host(&format!("100.64.0.{n}:28326"), "aa", "desk")
                .unwrap();
        }
        assert_eq!(store.known_hosts().count(), 20);
        assert!(store.forget_host("100.64.0.3:28326").unwrap());
        assert!(!store.forget_host("100.64.0.3:28326").unwrap());
        let on_disk = TrustStore::load(home.join("trust.json")).unwrap();
        assert_eq!(on_disk.known_hosts().count(), 19);

        let settings = Settings {
            new_desks: illum_ui::app::Permission::Never,
            max_fps: 60,
            ..Settings::default()
        };
        assert_eq!(
            ask(
                &pipe,
                &Request::Settings {
                    settings: settings.clone()
                }
            ),
            Ok(true)
        );
        assert_eq!(crate::app::load_settings(&home), settings);

        // Ending a session reaches the host's own hand for it, and says whether there was one.
        let end_of = |name: &str| {
            ask(
                &pipe,
                &Request::EndSession {
                    name: name.to_string(),
                },
            )
        };
        assert_eq!(end_of("laptop"), Ok(true));
        assert_eq!(end_of("nobody"), Ok(false));
        assert_eq!(*ended.lock().unwrap(), ["laptop", "nobody"]);

        // Something that is not a request gets an answer saying so, and the pipe lives on.
        {
            use std::io::{BufRead, BufReader, Write};
            let mut raw = loop {
                if let Ok(file) = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&pipe)
                {
                    break file;
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            raw.write_all(b"{\"ask\":\"become_admin\"}\n").unwrap();
            let mut said = String::new();
            BufReader::new(&raw).read_line(&mut said).unwrap();
            let reply: Reply = serde_json::from_str(&said).unwrap();
            assert!(!reply.ok && !reply.why.is_empty(), "{said}");
        }
        assert_eq!(
            ask(&pipe, &Request::Settings { settings }),
            Ok(true),
            "still served"
        );

        serving.abort();
        let _ = runtime.block_on(serving);
        assert!(
            ask(
                &pipe_name(&home.join("nobody"), false),
                &Request::Settings {
                    settings: Settings::default()
                }
            )
            .is_err(),
            "no host, no answer"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The pipe as the installed host opens it: closed to everyone but the system and one
    /// account. That account gets in; with nobody signed in, nobody but the system does.
    #[test]
    fn the_pipe_is_for_one_account() {
        let home = std::env::temp_dir().join(format!("illum-control-acl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let me = illum_win::own_user_sid().expect("this account");
        let request = Request::Settings {
            settings: Settings::default(),
        };

        let pipe = pipe_name(&home.join("mine"), false);
        let guest = me.clone();
        let serving = runtime.spawn(serve_for(
            home.clone(),
            pipe.clone(),
            Hands {
                end: Arc::new(|_| 0),
                update: None,
            },
            Some(move || Some(guest.clone())),
        ));
        assert_eq!(ask(&pipe, &request), Ok(true), "the account named gets in");
        serving.abort();
        let _ = runtime.block_on(serving);

        // Nobody signed in. (A test run as the system itself would get in; none is.)
        if !illum_win::runs_as_system() {
            let pipe = pipe_name(&home.join("nobody"), false);
            let serving = runtime.spawn(serve_for(
                home.clone(),
                pipe.clone(),
                Hands {
                    end: Arc::new(|_| 0),
                    update: None,
                },
                Some(|| None),
            ));
            let refused = ask(&pipe, &request).unwrap_err();
            assert!(refused.contains("does not answer"), "{refused}");
            serving.abort();
            let _ = runtime.block_on(serving);
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn requests_read_as_written() {
        let request = Request::Trust {
            change: Change::ForgetPeer {
                fingerprint: "ff".into(),
            },
        };
        let text = serde_json::to_string(&request).unwrap();
        assert_eq!(
            text,
            r#"{"ask":"trust","change":{"change":"forget_peer","fingerprint":"ff"}}"#
        );
        assert_eq!(serde_json::from_str::<Request>(&text).unwrap(), request);
        assert_ne!(
            pipe_name(Path::new("a"), false),
            pipe_name(Path::new("b"), false)
        );
        assert_eq!(pipe_name(Path::new("a"), true), r"\\.\pipe\Illumination");
    }
}
