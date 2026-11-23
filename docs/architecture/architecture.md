# Illumination architecture

Illumination is a remote-desktop program: it shows another machine's screen and sends it your
keyboard and mouse. This document is the entry point. It says what the program is for, which
processes it runs as, where their logs are, and how the code is split into crates. Each
subsystem has a document of its own.

**Section numbers are fixed.** Comments in the code cite this design as "architecture §3",
"§5.5" and so on. The numbers are kept across the documents, so a section that is missing here
is in another file:

| Section | Topic | Document |
|---|---|---|
| §1, §2, §3, §11, §12 | Goals, the design rule, processes, diagnostics, crates | this document |
| §4, §9, §10 | Listening, the connection, keys and trust, permissions, session policy | [network-and-trust.md](../network-and-trust/network-and-trust.md) |
| §5.1–§5.4, §5.8 | Capture, codecs, the media path, presentation | [video.md](../session/video.md) |
| §5.5–§5.7 | Adapting bitrate, frame rate and parity to the link | [adaptation.md](../adaptation/adaptation.md) |
| §6, §7 | Keyboard, mouse, the lock screen and permission prompts | [input.md](../session/input.md) |
| §8 | Clipboard and sound | [clipboard-audio.md](../session/clipboard-audio.md) |
| — | What is planned | [future-work.md](../future-work/future-work.md) |
| §14 | The app window, the tray, the session window | the code: `crates/illum-ui/src/app.rs`, `session_bar.rs` |

Words used throughout:

- A **desk** is a machine running Illumination.
- The **host** is the desk being shown and controlled. The **controller** is the desk you sit
  at; its window onto the host is the **viewer** or **session window**.
- A **session** is one connection from a controller to a host.
- The **home folder** holds a desk's key, the desks it knows and its settings. It is
  `%ProgramData%\Illumination` on Windows and `~/.illumination` elsewhere; `--home <DIR>`
  chooses another.

## 1. Goals and non-goals

**Goals**

- Remote desktop between your own machines, reached **by IP address**. The main case is a
  [Tailscale](https://tailscale.com) tailnet (a private network between your devices). A LAN
  or another VPN works the same way. The app takes IP addresses only; the command line
  (`illum view`, `illum connect`) also resolves a hostname.
- It should feel as good as AnyDesk on the same link: low delay, sharp text, correct colours.
- **One program for both sides.** Every install can control other desks and be controlled.
- Free and open: GPL-3.0-only, written in Rust.

**Non-goals**

- No ID server, no rendezvous server, no relay, no accounts. Getting through NAT is the
  network's job; Tailscale already finds a direct path or uses its own relay.
- No UPnP (asking the router to open a port) and no exposure to the public internet.
- No compatibility with Moonlight, Sunshine, RustDesk or AnyDesk clients.

**Platform.** Windows 10 and 11, x64, for both roles. This is the only platform where the
program works today. The protocol, the codecs, the adaptation, the window and the presenter
are platform-neutral code (FFmpeg, `wgpu`, `winit`, `egui`). Screen capture, input injection,
sound capture and playback, and the service exist for Windows only, in the `illum-win` crate.
macOS and Linux are a goal; see [future-work.md](../future-work/future-work.md).

## 2. The design rule

When sources disagree, **follow AnyDesk's observed behaviour.** The sources are AnyDesk,
Sunshine, RustDesk and general knowledge. AnyDesk has been stable for many users for a long
time. "Observed" means what its logs, sockets and documentation show. It is closed source, so
Illumination rebuilds behaviour and copies no code.

Three exceptions are deliberate:

1. **No relay and no ID.** That is the product's scope. AnyDesk's relay link and the layer
   that switches between relay and direct connection have no counterpart here.
2. **No UPnP.** AnyDesk tries to forward its port on the router. Illumination binds to chosen
   interfaces only (by default the machine's tailnet addresses).
3. **Pixels and sound travel over UDP and are never resent.** AnyDesk sends everything over one
   TCP connection. TCP resends lost data, and everything behind the lost piece waits.
   Illumination repairs loss with parity packets instead ([video.md](../session/video.md),
   [adaptation.md](../adaptation/adaptation.md)). Control messages, keys, buttons and the
   clipboard stay on TLS over TCP, as in AnyDesk.

## 3. Process model

There is one binary, `illum`. Its command line decides what a process is. Diagram:
[`processes.d2`](processes.d2) (render with `d2 processes.d2 processes.svg`).

### The processes

| Process | Command | Runs as | Lifetime | What it does |
|---|---|---|---|---|
| The app | `illum` (`illum --tray` starts it hidden in the tray) | The signed-in user | Until closed. One per user. | The window and tray icon: this desk's address, the known desks, settings, connecting out. Listens for incoming connections itself when no service is running. |
| Session window | `illum view <address>` | The signed-in user | One per outgoing session | Connects to a host, decodes, presents, sends input. The app starts one for each session; a display popped out of a session is one more. |
| The service | `illum service run` | The system account, session 0 | While the Windows service runs | Keeps one host running in the session that has the physical screen. Makes the Ctrl+Alt+Del call for it. Does nothing else. |
| The installed host | `illum --service --from-service` | The system account, inside the session at the screen | Started and restarted by the service | Listens, checks keys, serves every incoming session: capture, encoding, input, sound, clipboard. |
| Question window | `illum ask …` | The signed-in user | One question | Asks the person at the desk about a desk not seen before. Its exit code is the answer. |
| Console host | `illum --service` | Whoever starts it | Until stopped | The same host, run by hand in a terminal. Asks its questions in the terminal. For tests and development. |

Windows background: a *session* in Windows' sense is one sign-in's set of desktops and
windows. Services run in session 0, which has no screen. The *console session* is the one
attached to the physical screen and keyboard.

The flags `--backend` and `--control` are reserved for a finer split (a capture process per
session, a clipboard process per user). They are accepted and answer "not implemented yet".

### One host, three ways to run it

The listener and the session code are the same in all three (`accept_loop` and
`illum::host::serve_session`). What differs is who starts it and where its questions appear.

| | Listening process | Rights | A new desk is asked about in |
|---|---|---|---|
| Not installed | the app | the user's | a dialog in the app's window |
| Installed | the installed host | the system's | the question window (`illum ask`) |
| By hand | `illum --service` | the user's | the terminal |

With the user's rights the host cannot send input to windows that run as administrator, and
cannot follow the screen to the lock screen or a permission (UAC) prompt. The installed host
can do the first, and has the code for the second ([input.md](../session/input.md), §7).

### The installed desk

The installer copies `illum.exe` to Program Files and runs `illum service install`. After that:

- **The service keeps the host alive.** It looks twice a second for the session that has the
  screen. It starts the host there with its own system token moved into that session
  (`illum_win::start_in_session`). It starts the host again when it ends, at most once every
  three seconds. When the screen moves to another session (sign-out, switching users) it ends
  the host and starts a new one in the new session. The sessions served by the old host end;
  viewers reconnect.
- **The app does not listen while the service runs.** It shows what the host reports.
- **Closing the app stops the service; opening it starts the service.** An installed desk can
  be reached exactly while the app runs (window open or in the tray), or, with "Start on
  boot" on, from Windows' start before anyone signs in. Installing lets ordinary users start
  and stop this one service and change nothing else about it.
- **Ctrl+Alt+Del** can only be raised by a service (`SendSAS`). The host sets a named event,
  `Global\Illumination-SAS`; the service waits on it and makes the call
  ([input.md](../session/input.md)).
- **A new desk's question** is shown by a separate process that runs as the signed-in user,
  not by the host. A window with system rights on a user's desktop would be a way for any
  program there to gain those rights. The host starts `illum ask` with the user's token
  (`illum_win::start_as_session_user`) and reads the exit code: accepted, accepted this time
  only, declined, or declined for good. Nobody signed in, the window closed, or the connection
  given up: declined. A desk accepted before is not asked about, which is what lets it in with
  nobody there.

### How the processes share state

No pipe joins the service and the host, and none joins the host and the app for reading.
They share the home folder.

| File | Written by | Read by | Content |
|---|---|---|---|
| device key | host or app, once | all | This desk's identity ([network-and-trust.md](../network-and-trust/network-and-trust.md)) |
| `trust.json` | the listening process | all | Known desks, their keys and permissions. Re-read when its time stamp changes, before every look-up and every change. |
| `settings.json` | the listening process | all | Settings. The installed host reads it every second: what a connected desk may do (control, sound, clipboard), the idle limit, how new desks are treated. |
| `host-status.json` | the installed host, on every change | the app | Listening or not and why; who is connected, from where, since when; how an update the app asked for is going. |

**On an installed desk the home folder belongs to the system.** The host has system rights
and believes what is in the folder, including which desks get in without a question. So
installing makes the system and administrators the only writers; users can read. Uninstalling
gives write access back, for running the app without the service.

What a user's program wants changed, it asks the host to change, over a named pipe
(`illum::control`):

- The pipe is `\\.\pipe\Illumination`. It can be opened by the system and by the account
  signed in at the host's screen. The host answers only a program in its own Windows session.
- A request is one line of JSON and so is the answer. There are five kinds: a change to the
  known desks (`illum_core::store::Change`: remember a host's key, forget a desk, set a
  permission, rename, favourite, note a connection), saving the settings, ending the
  sessions of a connected desk, updating to the latest release, and stopping that update's
  download.
- The host serves one program at a time and creates the pipe anew for each. It refuses to
  create it if a pipe of that name already exists, and it re-creates an unused pipe every
  five seconds so that it is open for whoever signed in since.
- Programs do not call the pipe directly. The trust store routes its own writes through it
  (`TrustStore::write_through`), so the app, a session window and `illum forget` behave the
  same on an installed desk as on one that is not.

Files that belong to a user's program (its log, the result of its GPU probe, per-desk choices)
go to the home folder where it is writable and otherwise to `%LOCALAPPDATA%\Illumination`
(`own_folder` in `crates/illum/src/main.rs`).

### Updates

An installed desk updates itself from GitHub Releases (`crates/illum/src/update.rs`). Each
release carries three files: the installer, its signature, and `latest.json`, which says the
version, the release's notes, and where the installer and its signature are.

- **The app checks.** A few seconds after it opens, then once a day, and when the person asks
  in Settings, it fetches
  `https://github.com/Yooouuuuuuu/illumination/releases/latest/download/latest.json`. The
  request carries the program's name and version and nothing about the desk. Only a higher
  version is offered, and only when its installer's address is that version's file in this
  repository's releases. A desk run with `--home` does not check.
- **The host installs.** "Update now" asks the host over the pipe. The host refuses while a
  desk is connected (the app refuses while a session window is open), fetches the manifest
  itself, and downloads the installer into the home folder, which only the system writes.
- **Only a signed installer runs.** The signature is minisign's (Ed25519). The public key is
  compiled into `illum`; the private key signs releases (`illum-sign`) and stays on the
  machine that builds them. The signature's trusted comment names the installer's file, so
  one release's installer cannot be passed off as another's. A download that fails the check
  is deleted.
- **No administrator prompt.** The host has the system's rights, so it runs the installer
  silently. It leaves a marker in the home folder first. The installer stops the service
  (ending the host), waits for the app to close (the app closes when the host reports that
  the installer runs), replaces the program, and starts the service. The new service finds the
  marker and opens the app as the person at the screen. If the app has not closed after 30
  seconds, the installer starts the old service again and stops.

A desk that is not installed has no host to install for it; its dialog points to the release
page.

### Known limits

- **The desk's key is readable by every account on the PC.** The app needs it to connect out.
  Any account on an installed desk can therefore connect *as this desk* to the desks that
  accepted it. AnyDesk and RustDesk have the same property: one identity per machine.
- **The app does not check who serves the pipe.** A program of the user's own that created
  the pipe first could answer "done" without doing anything. It could not change the trust
  store, which only the system writes.
- **Several controllers can use the keyboard and mouse at once.** Each connected desk gets
  what its permission and the settings allow; there is no "one controller, the rest watch"
  rule yet.

### Other commands

| Command | Purpose |
|---|---|
| `illum connect <address>` | Connect, measure round-trip time, disconnect |
| `illum displays` | List this machine's displays |
| `illum codecs` | Show which video codecs this machine can encode and decode |
| `illum fingerprint` | Show this desk's key fingerprint |
| `illum list` | List the hosts this desk knows and the desks that may connect to it |
| `illum forget <address>` | Remove the remembered key of a host |
| `illum service install` / `uninstall` / `status` | Register, remove or report on the Windows service (the first two need an administrator) |
| `illum workload`, `illum report <dir>` | Measurement runs |

`illum --help` and `illum view --help` list every flag.

## 11. Diagnostics

**The numbers of the last second.** Each video frame carries its capture time and how long
the encoder took. The viewer adds its own decode and draw times and the time the frame was
shown. Once a second the session window's bottom line shows the host's address and three
numbers for the person: frames per second, bitrate and delay (the median from capture to the screen;
the network round trip until the two clocks agree), and packet loss when there is some. It
is one line, never two, cut short with "…" in a narrow window, and its words cannot be
selected. With several displays in the window ("All") it says how many after the address,
and the bitrate is theirs together; frames per second and delay are those of the window's
own display. The full set goes to the viewer's log every second: codec, step of the quality
ladder, frames per second, bitrate, delay, encode, decode and draw time, round-trip time,
input round trip, packet loss and packets repaired. A viewer drawing on the CPU has no bars
and shows the full set in its title bar.

**The viewer says what it is waiting for** instead of freezing silently: "getting a
fresh picture from the host…" after 2 s without the keyframe it asked for, and a stall notice after 3 s without media
while the control connection is alive ([adaptation.md](../adaptation/adaptation.md)).

**Logs.** Every process writes to the console (stderr) at info level, or debug with `-v`, and
appends to a file of the day at info level, or debug while "Detailed logs" is on in Settings
(General), for finding a problem. A change of that switch applies at once to the app and the
host, and to session windows opened after it:

| Process | File |
|---|---|
| The app | `logs\app.2026-11-23.log` |
| Session window | `logs\viewer.2026-11-23.log` |
| The service | `logs\service-run.2026-11-23.log` |
| Host (installed or by hand) | `logs\service.2026-11-23.log` |
| Question window | `logs\ask.2026-11-23.log` |
| Other commands | `logs\frontend.2026-11-23.log` |

The `logs` folder is in the home folder where the process can write there. On an installed
desk that is true for the service and the host; the user's processes log to
`%LOCALAPPDATA%\Illumination\logs`. A new file starts each day at midnight UTC (the lines'
times are UTC too); processes of one role share it. A file not written for 7 days is
deleted when a process opens its own.

Each line starts with the component it comes from: `app`, `service`, `session`, `net`,
`identity`, `media`, `codec`, `capture`, `audio`, `input`, `clipboard`, `viewer`, `workload`.
The GPU libraries (`wgpu`, `naga`) are logged at warning level and above only.

**Reading a session back.** The host logs every change of ladder step at info level with its
reason, and one line per second at debug level (with "Detailed logs"): frames shown and on time, datagrams received
and lost, frames lost, packets repaired, round-trip time, grade. With `view --report <file>`
the viewer also writes these per-second records as JSON, with the host's own per-second
numbers; `illum report` turns a folder of them into a table.

**State of an installed desk.** `illum service status` says whether the service is installed
and running and what its host reports.

## 12. Crate layout

The workspace has eight crates under `crates/`.

| Crate | Contents |
|---|---|
| `illum` | The binary. Command line and process roles (`main.rs`), the app's logic behind its window (`app.rs`), the service and the installed host (`installed.rs`), the pipe (`control.rs`), updates (`update.rs`). As a library: the host side of a session (`host.rs`), the viewer side (`viewer.rs`), the host's input rules (`input.rs`), sound, clipboard and measurement glue. |
| `illum-core` | What both ends agree on, with no I/O beyond the trust store's file: protocol messages and channels, capabilities, input messages, the neutral key codes and their mapping (`keymap`), clipboard messages, the trust store (`store`), the operating system's name. |
| `illum-net` | The connection: choosing addresses to bind, TCP with TLS 1.3 and certificates on both sides, the device key, the hello and key check, channel framing with priorities, and the UDP media path with keys exported from the TLS session. |
| `illum-media` | Platform-neutral media: the pixel format, codec interfaces, the lossless tile codec, packets and parity (FEC), the pacer, clock synchronisation, the adaptation controllers and the simulated link that tests them, cursor shapes, scaling, the colour test pattern, the measurement workload. |
| `illum-ffmpeg` | AV1 and H.265 encoding and decoding through FFmpeg: the probe of what this machine's hardware can do, low-delay encoder settings, GPU decoding through FFmpeg's hardware paths, and Opus for sound. |
| `illum-win` | Everything Windows-specific: screen capture (Desktop Duplication, with a GDI fallback), input injection, the system-key hook, following the screen to the lock screen and permission prompts, WASAPI sound, keep-awake, the service, starting processes in a session, pipe security, `SendSAS`, the one-app-per-user rule. Compiles to nothing on other systems. |
| `illum-ui` | Windows the person sees, on `winit`: the session window with its `wgpu` presenter and CPU fallback, the controller's keyboard and mouse handling, the session bars, the tiles for several displays, the app window (`egui`), the tray icon, the measurement workload's window. |
| `illum-sign` | The release tool: makes the update key and signs a release's installer, writing its signature and `latest.json`. Not part of the program. |

Dependencies point one way. `illum-core` and `illum-media` depend on no other crate of the
workspace. `illum-net` builds on `illum-core`, `illum-ffmpeg` on `illum-media`, and `illum-ui`
and `illum-win` on both. `illum` uses all six; `illum-sign` uses none. A new platform adds a
crate beside `illum-win`.
