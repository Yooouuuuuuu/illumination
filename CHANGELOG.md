# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

Entries record *what shipped*, kept simple. The *why* behind a design lives in
the docs: the design documents and the decision ledger in
[docs/README.md](docs/README.md).

## [Unreleased]

## [0.16.13] - 2026-11-23

### Changed

- **Logs are smaller and kept a week.** The files are written at info level; "Detailed
  logs" in Settings, General, turns on the debug level for finding a problem. Each role
  writes one file a day, `viewer.2026-11-23.log` (dates in UTC), and files not written
  for 7 days are deleted, the old `viewer.log` and `service.log` with them.

## [0.16.12] - 2026-11-23

### Changed

- **"1 desk connected here"** in the main window's status and the tray's tooltip (it said
  "1 desk connected"), so it reads apart from "Connected to 1 desk".

## [0.16.11] - 2026-11-23

### Changed

- **"Update now"** in Settings, About, once an update is found (it said "Update…"). It
  opens the update as before.

## [0.16.10] - 2026-11-23

### Changed

- **Settings: Save on the left, Discard on the right**, and after saving a green "Saved"
  where the count of unsaved changes was, until the next change.

## [0.16.9] - 2026-11-23

### Changed

- **"Exit button transparency" in Settings is a number you type**, like the delay limit:
  0 % is solid, 100 % hidden, 70 % by default (the same look as before). It replaces the
  slider "Exit tab when not pointed at".

## [0.16.8] - 2026-11-23

### Changed

- **Plainer, more formal wording.** A session that ends says "Disconnected by the host after 30 minutes without input." or
  "Disconnected manually at the host."; Settings, locked, says "Locked while 1 desk is
  connected. This keeps a connected desk from changing who may connect here."; an update
  waiting for connections says "Updating ends all connections. … Disconnect them first."
  Desks connected here are counted, not named, in these sentences.

## [0.16.7] - 2026-11-23

### Changed

- **The session window shows the host's address alone.** The title reads
  "Illumination — 100.100.101.2" and the bottom line starts "100.100.101.2 · 30 fps · …";
  the desk's name is no longer shown there.

## [0.16.6] - 2026-11-22

### Changed

- **The session window names the desk as you do, with its address.** The title reads
  "Illumination — 100.100.101.2 (LAPTOP)" and the bottom line "LAPTOP · 100.100.101.2 ·
  30 fps · …". The name is your own name for the desk when you gave it one, else the
  machine's, and an open window follows when you rename the desk.

## [0.16.5] - 2026-11-22

### Added

- **The main window shows that you are connected out.** While session windows are open
  to other desks, a blue pill beside the status says "Connected to 1 desk" or
  "Connected to N desks"; a click brings those windows to the front.

### Changed

- **The status pill gives the number, not a name.** A desk connected here shows as
  "1 desk connected" instead of its name, as the tray says it. The "Connected here"
  list still has the names.

## [0.16.4] - 2026-11-22

### Changed

- **A clearer note while the picture is refreshed.** When a lost frame makes the viewer
  ask the host for a whole new picture and it takes over 2 s, the note now says
  "getting a fresh picture from the host…" instead of "waiting for a keyframe from the
  host…".

## [0.16.3] - 2026-11-22

### Changed

- **The session window's bottom line is short and plain.** It shows the host's name,
  frames per second, bitrate and delay, and loss only when there is some, for example
  "LAPTOP · 30 fps · 12.3 Mb/s · delay 18 ms". It stays on one line, cut short with
  "…" in a narrow window, and its words can no longer be selected. Codec, quality step,
  encode, decode and draw times, round trip, input and repaired frames are in the
  viewer's log every second.

## [0.16.2] - 2026-11-22

### Added

- **The "Exit" tab can stay faintly visible.** With the bars hidden (Focus, full screen)
  the tab at the top edge now shows at 30 % while the mouse is elsewhere, and fully when
  it is there. "Exit tab when not pointed at" in Settings sets it from 0 to 100 %.
- **Hotkeys for the window modes.** Ctrl+Alt+Shift+F10 switches Focus on and off, and
  Ctrl+Alt+Shift+F11 full screen; switched off, the bars come back. Like
  Ctrl+Alt+Shift+F2, they are never sent to the host.

## [0.16.1] - 2026-11-22

### Changed

- **A session that ends keeps its window and says why.** When the host ends the session
  (no input for the idle time, or the person at the host ends it), the window no longer
  closes: the last picture stays, dimmed, with a card such as "No input for 30 min, so
  the host ended the session." and the buttons Reconnect and Close. A lost link with
  reconnecting off is shown the same way. Popped-out displays close, and "All" goes back
  to one display.

## [0.16.0] - 2026-11-22

### Changed

- **Who may connect is locked while a desk is connected here.** A desk controlling
  this one could open the app here and let itself or any desk in for good. While a
  desk is connected, Settings greys out Incoming and Trusted desks and says why, and a
  desk's permission and Forget are greyed out too. The installed host also refuses
  these changes, whichever program asks. Names and favourites can still change.

## [0.15.3] - 2026-11-22

### Added

- **The version** beside the app's name at the top of the main window, small and
  grey (Home only).

### Changed

- **"All" and popped-out displays do not mix.** While "All" is on, More offers no
  "Pop out display"; turning "All" on closes the displays popped out of the window,
  and the tiles show them instead.

### Fixed

- **The displays of a session window share one bitrate limit.** Each display, a
  tile of "All" or a popped-out one, had a limit of its own and stepped up and down
  as if the link were its alone, so together they overloaded it and lagged. Now
  "Max bitrate" in Settings is the limit for everything the window shows, split by
  the pixels each display sends: a small tile spends what its size needs (a quarter
  of the width, a sixteenth of the limit), and all of them step down and up together.
- **A re-sent keyframe is no longer shrunk twice.** On a still screen, a frame the
  viewer lost was sent again from a picture already made smaller, at a smaller size
  still, which restarted the encoder twice and sent two full pictures.

## [0.15.2] - 2026-11-19

Released.

### Fixed

- **An update no longer stops at 100 %.** The app missed the moment its
  update's installer started, so it stayed open at 100 % and the installer,
  which waits for it, gave up after 30 seconds and started the old version
  again. The app now sees the installer start and closes. The installer also
  closes an app that is still open after 5 seconds, so an update from 0.15.1,
  whose app has the old fault, goes through too.

## [0.15.1] - 2026-11-19

Released.

### Fixed

- **A connected desk goes by the name you gave it.** The top of the main
  window ("Laptop is connected"), the Connected here list and the update
  dialog showed the name the other desk gave itself, not the one you set in
  Edit.

### Changed

- **The update dialog shows the release's notes as the release page has
  them**, with their bold, and says the version you are on ("Current:
  v0.15.1"). It showed the changelog entry. `illum-sign release` now takes the
  notes from the release's section of the release history and also writes
  them to `release-notes.md`, the text of the GitHub Release.

## [0.15.0] - 2026-11-19

Released: the first release. It also ships 0.13.0 to 0.14.8, which were not
released on their own.

### Added

- **Updates.** The app looks for a newer release when it starts and once a
  day, and Settings, About has **Check for updates**. When there is one,
  **Update to X** appears in the top bar. Its dialog shows the release's notes;
  **Update now** downloads the installer, checks its signature, and installs
  it with no administrator prompt. Illumination closes and opens again with
  its desks and settings. Nothing is downloaded before **Update now**, and
  updating waits while a desk is connected or a session window is open.
- `illum-sign`, which makes the update key and signs a release
  (`scripts/build-installer.ps1 -Release` writes the installer, its signature
  and `latest.json`).

### Fixed

- **Max bitrate** above 20 Mb/s was held at 20 Mb/s by the host. It now goes up
  to the 200 Mb/s that Settings offers, and the connection still steps down to
  ordinary rates when the link cannot carry it.
- The message when the Illumination service is not running says how to start
  it again.
- The command line's help named the old port, 7171.

### Changed

- The README is shorter, with pictures, and the technical documents in
  [docs/](docs/README.md) were reorganised.

## [0.14.8] - 2026-11-18

Not released.

### Changed

- **Connecting to a desk that is already open** brings its session window to
  the front (out of the taskbar if it was minimized) instead of opening a
  second one. The desk is recognised by the address you connect to.
- **Max bitrate and Delay limit** (Settings, Connection) are typed: a click
  puts the cursor in the field, dragging no longer changes the number, and the
  pointer over them is the text pointer.

## [0.14.7] - 2026-11-18

Not released.

### Added

- **Trusted desks**, a section of Settings: the desks that are always allowed
  to connect here, each with Remove (which puts it back on the default).
- **This desk** says what happens to a new desk that connects ("New desks are
  asked first", and how many desks are always allowed); a click opens Settings
  at Incoming. A button beside the address copies it.

### Changed

- **Desk cards** have a strip at the bottom with the desk's own permission in
  words ("Always allow", "Ask", "Never allow", or "Default permission") and the
  button for what can be done. Online and offline are a dot and a word.
  Pointing at a card thickens its outline.
- **Favorites**: a favorite's card is cream. The star is gone from the card;
  "Add to favorites" and "Remove from favorites" are in the card's menu, and no
  longer in Edit.
- **Settings**: "Outgoing" is now "Connection" and comes before Incoming.

### Removed

- The second address line ("also ...", this desk's IPv6 address) under This
  desk.

## [0.14.6] - 2026-11-18

Not released.

### Added

- **"Always allow" as the setting for new desks**, beside Ask and Never allow
  (Settings, Incoming). Ask stays the default. With "Always allow", any desk
  that can reach this one connects without a question; the top of the main
  window then reads "Open to any desk".
- **Keyboard and pointer are remembered per desk.** What you choose under More
  in a session window is used again the next time you connect to that desk.

### Changed

- **Keyboard and pointer left Settings.** Both start as Remote, and both read
  "Remote | Local" (the pointer said "This desk's | The host's"). The pointer's
  default was this desk's own. `--pointer` takes `remote` and `local`; `desk`
  and `host` still work.
- **Permission wording**: "Always allow", "Ask", "Never allow". Editing a desk
  asks "When it connects here", and "Default" says what the default is.
- **Desk cards**: double-click a card to connect. The button at its bottom
  right opens Connect and Edit. A desk's own permission is a mark beside its
  address (hover for the words) instead of a line of text.
- **Settings menu** lists the four sections only.
- The state at the top of the main window ("Ready for connections") is smaller
  than the Settings button, and the two cards of the first row lost their
  empty space.

Verified with cargo fmt, cargo clippy and cargo test (a new desk under each of
the three settings, an old settings file read as before) and by pictures of
the windows. Not yet tried by hand: the double click, the card's menu, and the
remembered keyboard and pointer.

## [0.14.5] - 2026-11-18

Not released.

### Added

- **All displays in one window.** With a host that has several displays, the
  Display row has "All": the displays are shown as tiles, two side by side,
  three or four in a grid (four at most). Each display is sent at about the
  size it is shown, so this costs about as much as one display.
- **Control | Arrange**, next to it. In Arrange the mouse is the window's: drag
  a tile onto another to swap them, drag the line between tiles to give one
  more room. Nothing is sent to the host meanwhile. The arrangement is
  remembered for that desk.

### Changed

- A host sends a display smaller when the controller asks for it so (it did
  only in measurement runs). Both desks need this version for "All" to save
  anything; with an older host the displays come at full size.

Verified with cargo fmt, cargo clippy and cargo test (the tile layout, a
display sent at the size asked for, two pictures in one frame) and by a
picture of the bar while arranging. Not yet tried with a real host that has
two displays.

## [0.14.4] - 2026-11-18

Not released.

### Added

- **Pop out a display.** With a host that has several displays, More has "Pop
  out display 2" (and so on): that display opens in a window of its own, which
  you can put on another monitor. It has the main window's permission, no sound
  of its own, and closes when the main window closes.

### Changed

- A desk with several windows open to this one is listed once under "Connected
  here", and "Disconnect when idle" counts its input from any of them.

Verified with cargo fmt, cargo clippy and cargo test. Not yet tried with a real
host that has two displays.

## [0.14.3] - 2026-11-18

Not released. Step four of the 0.14 redesign: the session window.

### Added

- **Three window modes**, at the right of the session bar: Detail (both bars),
  Focus (the picture alone) and Full screen. With the bars hidden, move the
  mouse to the top edge for a small "Exit" tab; it can be slid sideways.
- **Lock the host**, under More. Win+L pressed while controlling locks your own
  PC, so this is the way to lock the other one. Both desks need this version.
- A warning (the host is about to disconnect, the link dropped) now also shows
  when the bars are hidden, as a small note over the picture.

### Changed

- **The session bar is thinner and holds less**: displays, Fit / Original,
  More, and the window mode. Keyboard, pointer, Send Ctrl+Alt+Del, Lock and
  Disconnect are under More. The bottom line shows the host's name and address
  with the numbers.
- The bars are pale cream with thin lines, and on Windows 11 the window's own
  title bar is the icon's pale yellow, on the app's window too.

Verified with cargo fmt, cargo clippy and cargo test, and by pictures of the
bar, the open menu and the Exit tab. Not yet used in a real session.

## [0.14.2] - 2026-11-18

Not released. Step three of the 0.14 redesign: running, hidden, closed.

### Added

- **A tray icon.** Minimizing sends the app to the tray instead of the taskbar.
  A click opens the window; the menu has Open, "Connected here" and Quit. The
  icon turns yellow while a desk is connected to this one.
- **"Connected here"**: click the state at the top right, or the tray menu, to
  see each connected desk, its address and for how long, with Disconnect.
- **Start on boot** (Settings, on by default): the app starts in the tray when
  you sign in, and an installed desk can be reached at the sign-in screen.
- Opening the app while it is running brings its window up instead of starting
  a second one.

### Changed

- **Closing the app closes everything.** The X, or Quit in the tray menu, ends
  every connection and stops the service: nothing of Illumination is left
  running and this desk cannot be reached until you open the app again. If
  something is connected it asks first and offers the tray instead.
- Opening the app starts the service again, without an administrator prompt.
  Installing this version sets that up; a desk installed with an earlier
  version needs the new installer run once.

Verified with cargo fmt, cargo clippy and cargo test, by pictures of the new
dialogs, and by starting the app in the tray on this desktop. Closing and
reopening with the installed service has not been tried: it needs this
version's installer.

## [0.14.1] - 2026-11-18

Not released. Step two of the 0.14 redesign: the app window.

### Changed

- **One list of desks, in three tabs**: Favorites, Recent, Came in. A desk is
  one card whichever way you met it, showing its system, your name for it,
  whether it is online, and its permission if it has one of its own.
- **Edit a desk**: give it your own name, set its permission (default, always,
  ask, never), star it, or forget it.
- **Settings is rebuilt**: a menu of every setting on the left, and nothing
  applies until you press Save in the top bar. Outgoing has "Reset to defaults".
- **This desk's key moved to Settings.**
- **Colours mean one thing each**: green is open or allowed, blue is waiting
  for an answer, coral is closed or refused. The question about a new desk and
  the state at the top right follow them.
- "Allow incoming connections" is no longer a switch; an open app listens. Use
  a desk's permission, or the default permission "Never", to keep desks out.
- The "Trusted desks" list in Settings is gone: those desks are in "Came in"
  with "Permission: always".

Verified with cargo fmt, cargo clippy and cargo test, and by pictures of every
screen.

## [0.14.0] - 2026-11-18

Not released. The first of four steps of the 0.14 redesign: the rules. The windows still
look as before; the new ones come in 0.14.1 to 0.14.3.

### Added

- **A session with no input ends by itself.** The host disconnects a desk that
  has sent no keyboard or mouse for 30 minutes, after a one-minute warning in
  its session window. Settings, "When another desk connects here": 10 or 30
  minutes, 1 or 2 hours, or never. A view-only session counts the same.
- **"Never" for a desk.** The question about a desk now has four answers: No,
  This time, Always, and "Never for this desk", which refuses it from then on
  without asking.
- **Refuse desks that are not known.** Settings: a desk without a permission of
  its own is asked about, as before, or refused without a question.
- Each desk now tells the other what system it runs ("Windows 11"); the
  question about a new desk shows it.

### Changed

- **No question on your first connection to a desk.** Its key is remembered as
  it is met. A desk whose key later changes is still refused.
- **The app connects to IP addresses only.** A name is turned down with a hint.
- A desk that does not answer now says "No answer from 100.100.101.2. It may be
  off, or not accepting connections." instead of "timed out during TCP connect".
- The question's buttons read No / This time / Always.

Verified with cargo fmt, cargo clippy and cargo test.

## [0.13.8] - 2026-11-17

Not released.

### Fixed

- **Locking the host no longer freezes the session.** With a session open,
  locking the host (or signing back in) stopped the picture until the
  controller reconnected. Windows refuses a screen capture at the moment it
  changes screens, and that one refusal ended the capture just before it would
  have followed to the lock screen. A refused capture now looks where the
  screen went and carries on there.

Verified with cargo fmt, cargo clippy and cargo test. The fix follows from the
host's log of the failed try; it has not been tried on a lock screen yet.

## [0.13.7] - 2026-11-17

Not released.

### Changed

- **The pointer you see is this desk's own.** The host now says which pointer it
  is showing (arrow, text, hand, resize, wait, ...) and your own pointer of that
  kind is shown, in your size and scheme. A program's own pointer, such as a
  brush, still comes as the host's picture. Settings and the session window
  offer the other way too: "The host's pointers", the host's pictures for all of
  them. Both desks need this version; against an older host you get the host's
  pictures.
- The "Embedded" pointer is no longer a choice. A setting or a command line that
  still says `native` or `embedded` reads as `desk` or `host`.

### Fixed

- **The text pointer no longer disappears over a dark window.** Windows draws it
  as the opposite of what is under it, which cannot be copied; it was drawn
  black, and was invisible over a dark terminal. Where the host's picture is
  used it is now light with a dark edge.

Verified with cargo fmt, cargo clippy and cargo test, and on this desktop's real
screen: both ways of capturing name the pointer on screen (the arrow).

## [0.13.6] - 2026-11-16

Not released.

### Added

- **Ctrl+Alt+Del on an installed host.** The session window shows a
  "Ctrl+Alt+Del" button when the host is installed and you control it; it
  brings up the host's Ctrl+Alt+Del screen. Written and unit-tested, not yet
  tried on a real session: it waits with the lock-screen test.

### Fixed

- The session window's "Disconnect" button no longer covers its neighbour when
  the top bar is full; it moves to a second row.

Verified with cargo fmt, cargo clippy and cargo test.

## [0.13.5] - 2026-11-16

Not released.

### Added

- **Your desks show which ones are online.** Each card says "Online" or
  "Offline" and lights or darkens its little screen. The app looks every
  twenty seconds by knocking on the desk's port; nothing goes through a server.

### Changed

- A host no longer writes a warning to its log for every connection that left
  without saying anything (which is what such a knock is).

Verified with cargo fmt, cargo clippy and cargo test.

## [0.13.4] - 2026-11-16

Not released.

### Fixed

- **A desk disconnected from the host stays disconnected.** The host closed
  the connection right after its goodbye; while the controller was sending
  (it always is), Windows turned that into a reset that could swallow the
  goodbye, and the controller would have reconnected as after a dropped link.
  The host now waits for the controller to hang up first.

Verified with cargo fmt, cargo clippy and cargo test (the whole suite three
times; the session-end test 25 times, where it failed about one run in ten
before).

## [0.13.3] - 2026-11-16

Not released.

### Added

- **Switch between the host's displays.** When the host has more than one, the
  session window's top bar shows "Display 1 2 …"; choosing one shows that
  display and sends the keyboard and mouse there. The choice is kept through a
  reconnection. Both desks need this version.

Verified with cargo fmt, cargo clippy and cargo test: a host with two test
displays, the viewer switching to the second and back. Not yet tried on the two
real displays of the desktop.

## [0.13.2] - 2026-11-16

Not released.

### Added

- **Disconnect a desk that is connected to this one.** The home screen lists
  each connected desk with a "Disconnect" button. Its window closes and it does
  not reconnect by itself.
- **Accept a new desk for this time only.** The question about a new desk now
  has "This time" next to "Accept and trust": it is let in now and not
  remembered. A dropped link within two minutes does not ask again; being
  disconnected from here does.
- **Choose what a connected desk may do.** Settings has "When another desk
  connects here": use the keyboard and mouse, hear this desk's sound, share the
  clipboard. A desk connected at that moment keeps what it had until it
  connects again.

### Fixed

- **"Allow incoming connections" can be switched off and on again.** A desk
  that stopped listening never gave its port back, so switching it on again
  failed with "another Illumination is already listening" until the app, or
  the installed service, was restarted. Present since 0.5.0.

Verified with cargo fmt, cargo clippy and cargo test, on one PC: a desk let in
for this time, sent away, asked about again, back without a question after
leaving by itself, the sound switched off for the next connection, and incoming
switched off and on while a desk stays connected.

## [0.13.1] - 2026-11-16

Not released.

### Changed

- **An installed desk's files can no longer be changed by other accounts of the
  PC.** Installing now makes the home folder (`%ProgramData%\Illumination`) the
  system's: users can read it, not write it. The app asks the service to save
  the settings, to remember or forget a desk and to forget a trusted desk, over
  a local connection that only the system and the person signed in at the
  screen can open. This closes what 0.13.0 listed as known.
- The app's and the session windows' logs of an installed desk are now in
  `%LOCALAPPDATA%\Illumination\logs`. The service's and the host's stay in the
  home folder. An Illumination that is only run keeps everything where it was.
- Uninstalling gives the home folder back to the PC's users.

### Known

- Not yet tried with the installed service on either desk; the tests cover the
  asking, the answering and who is let in. The lock screen is still untried.

Verified with cargo fmt, cargo clippy and cargo test.

## [0.13.0] - 2026-11-16

Not released: no tag and no installer published until both desks confirm it.

### Added

- **Illumination can be installed.** `Illumination_0.13.0_x64-setup.exe` (built
  with `scripts/build-installer.ps1`) puts the program in Program Files, adds it
  to the Start menu and the desktop, and installs a Windows service. It asks for
  an administrator once. Uninstalling removes all of it and keeps this desk's
  key, its desks and its settings.
- **An installed desk can be reached in windows that run as administrator.**
  The service keeps the host running with system rights in the session at the
  screen, so typing, clicking and scrolling arrive there too. An Illumination
  that is only run, not installed, works as before, with the limits it had.
- **An installed desk can be reached while the app is closed**, and after a
  restart before anyone opens it. A desk you accepted before connects without
  anyone there. A new desk is still asked about, in a small window of its own.
- **The lock screen, the sign-in screen and Windows' permission prompts** are
  followed by an installed host: it captures and types where the screen is.
  Written, not yet seen working.
- `illum service install`, `uninstall` and `status` for doing the same from a
  terminal opened as administrator.

### Changed

- With the service running, the app no longer listens itself. "Allow incoming
  connections" switches the service's listening, and the status in the window
  is what the service reports.
- The app and the host notice each other's changes to the list of desks: a desk
  forgotten in the app is refused by the host at its next connection.

### Known

- On a PC with several accounts, another account can change this desk's files
  in its home folder. To be closed before this is released (architecture §3).

## [0.12.8] - 2026-11-16

### Fixed

- **A host whose front window runs as administrator now says so.** Windows
  keeps the input of an ordinary program out of such a window, and it does so
  without reporting an error, so the check added in 0.12.6 never noticed:
  typing, clicking and scrolling did nothing and no reason was shown. The host
  now looks at the window in front itself and tells the controller. Reaching
  such windows needs the host to run as a service, which is not built yet;
  until then, start the host as administrator or bring an ordinary window to
  the front.

## [0.12.7] - 2026-11-15

### Fixed

- **The keyboard no longer gets stuck after moving to another program.** The
  session window switched its key grab on and off when it was told it gained or
  lost the focus, and those reports sometimes came late or never. Missed one
  way, the grab stayed on after you moved to another program: the keyboard
  seemed dead there and the keys went to the host. Missed the other way, the
  window was in front but nothing was grabbed. The grab now asks Windows, for
  every key, which program is in front.

### Changed

- The session log says more about input, in counts only: for each time the
  window was in front, how many key events, clicks and moves were sent, how
  many the host acknowledged and when last, and how many key events reached
  the window itself. It also records the host's version and what it offers.

## [0.12.6] - 2026-11-15

### Added

- **The session says why input is not arriving.** When the host is on its lock
  screen, sign-in screen or a Windows permission prompt, or the window in front
  on the host runs as administrator, Windows quietly drops the input a host
  program sends, and the session just looked stuck. The host now notices,
  writes the reason to its log, and tells the controller, which shows it in the
  status strip and writes it to its own log; the same when input arrives again.
  This names the problem, it does not get past it: that needs the host to run
  as a system service, which is planned. Both sides need this version.

## [0.12.5] - 2026-11-15

### Fixed

- **No more false "video stalled; reconnecting…" on a still screen.** A host
  whose screen hardly changes sends a frame every few seconds. The viewer
  warned after 3 seconds without video if the host had reported sending
  anything in the last 5, so every such gap raised the notice and asked the
  host for a full frame: 368 times in a 22-minute idle session. It now warns
  only when the host reports video sent after the last video that arrived.

### Changed

- The session log notes when the window gains focus ("Focus came."), next to
  the existing count of key presses when focus leaves.

## [0.12.4] - 2026-11-13

### Fixed

- **The keyboard grab now holds: Win, Alt+Tab and typing go to the host only.**
  Through 0.12.3 the Windows key still opened the Start menu on the controlling
  machine and its input method still composed what was typed. The hook that
  takes the keys was installed and working, but Windows never called it:
  Windows skips a program's own keyboard hook while that program reads the
  keyboard raw and has the focus, and the window library switched raw reading
  on whenever the session window had focus. The session window no longer asks
  for raw input. Measured in a live session on one machine: before, the hook
  took 0 of 10 test presses; after, 10 of 10.

## [0.12.3] - 2026-11-13

### Fixed

- A test of the app (two desks connecting on one machine) failed about one run
  in six on Windows, and 0.12.2 was released with it failing. The app was not
  at fault: the test took a port number the system called free, but Windows
  keeps blocks of numbers back per protocol, and a number free for TCP could
  be refused for UDP (or the other way round). The test now picks from a
  different range and tries both. 200 runs in a row pass.

## [0.12.2] - 2026-11-13

### Fixed

- **The taskbar button shows the app's icon.** It showed a generic one, though
  the window's title bar had the right icon: the taskbar asks a new window for
  its icon as it appears, the window was still busy setting up the GPU, and
  the taskbar never asks twice. The app window and the session window now set
  their icon once more after their first frame, which makes it look again.

## [0.12.1] - 2026-11-13

### Added

- **The session window has its bars.** On top: who you are connected to,
  Fit / Original size, the keyboard mode (Remote / Local / Auto), the pointer
  mode (Native / Embedded), Full screen and Disconnect. At the bottom: the
  codec, the quality in use and the last second's numbers, which used to sit
  in the title bar, or a notice such as "connection lost; reconnecting…".
  The picture is fitted between the two.
- Keyboard and pointer mode can now be changed while connected; before, they
  were fixed when the session started.
- The bars are for the mouse only: every key still goes to the host.

### Changed

- With the bars, the window's title is just the host's name. A machine with no
  usable GPU gets no bars and keeps the numbers in its title bar.

## [0.12.0] - 2026-11-13

### Added

- **The app.** `illum` with no arguments now opens a window instead of a
  message saying there isn't one:
  - **This desk:** the address to give to the other machine, this machine's
    key, and "Allow incoming connections". While it is ticked the app listens,
    as `illum --service` does.
  - **Remote desk:** an address and Connect. Problems (nobody there, refused,
    a changed key) are said in the window. The session opens in a window of
    its own.
  - **Your desks:** every machine you have connected to, with Connect and
    Forget.
  - **Settings:** highest bitrate and frame rate, delay limit, scaling,
    reconnecting, keyboard and pointer mode, sound, clipboard, and the list of
    machines trusted to connect here. Kept in `settings.json`; they apply to
    the next connection.
  - **The two questions about keys** are dialogs now: "Is this …?" the first
    time you connect to a machine, and "… wants to connect" when an unknown
    machine reaches yours. The taskbar button flashes when one is waiting.
- Started by a double-click, the app no longer leaves an empty console window
  next to it.
- `illum_ui::app_window::snapshot` draws the interface to a picture without a
  window; the tests keep one of each screen in `target/app-pictures`.

### Changed

- The interface is drawn with `egui` on the window and GPU stack the viewer
  already used; `wgpu` went from 26 to 30 for it. The viewer's picture is
  unchanged (its offscreen colour tests pass as before). Building now needs
  Rust 1.87 or newer.
- Log files no longer fill with the GPU libraries' shader commentary: from
  those, only warnings and errors are kept.
- The app's log is `app.log`.

### Not yet

- The session window has no toolbar yet (display switch, modes, full screen,
  disconnect): next.
- Accepting a machine still means trusting it from then on, with everything
  allowed; accept-once and per-connection permissions need a protocol change.
- Don't run `illum --service` and the app on the same machine: they want the
  same port, and the app will say it cannot listen.

## [0.11.13] - 2026-11-13

### Added

- **The app has an icon:** two overlapping screens, navy outlines on a soft
  blue tile. `illum.exe` carries it (Explorer, the taskbar) and the viewer
  window shows it in its title bar. The drawings are in `assets/icon` (SVG
  masters, PNGs from 16 to 1024 px, the Windows ICO); its four colours are the
  base colours of the interface to come.

## [0.11.12] - 2026-11-12

### Changed

- **The viewer says which build it is and what it does with the keyboard** when
  it connects (`Illumination 0.11.12. Keyboard: every key goes to the host
  while this window has focus.`), and `viewer.log` records, each time the
  window loses focus, how many key presses went to the host and how many
  stayed on this machine — counts only, never the keys. The host's log names
  the viewer's version. A report that "the keys still act here" can now be
  read against what actually ran.

### Fixed

- The keyboard grab also takes keys that another program re-injects on your
  machine (a key remapper, an on-screen keyboard, a vendor hotkey tool). Until
  now every injected key was left to your own machine; only what Illumination
  itself injects is left alone now, which is how RustDesk tells them apart.

## [0.11.11] - 2026-11-12

### Fixed

- **The keyboard grab now holds: it is built the way RustDesk builds it.** In
  0.11.10 the Windows key still opened the Start menu on your own machine
  first. The hook sat on the window's thread, and Windows silently removes a
  keyboard hook whose thread answers late (that thread also sets up the GPU
  and draws). The hook now has a thread of its own that does nothing else,
  as in RustDesk.
- Copied from RustDesk as well: a grabbed key *press* is kept from your
  machine, its *release* still reaches it (so nothing stays stuck locally),
  and CapsLock / NumLock keep working on both machines.
- **Your local input method is no longer switched off** while grabbed. Its
  keys simply never reach it, so it has nothing to compose; the language
  switch and its indicator are left alone.
- Holding a key under the grab repeats once (the host's repeat), not twice.
- `Ctrl+Alt+Shift+F2` (fit / original size) works under the grab again and is
  not sent to the host.

## [0.11.10] - 2026-11-11

### Changed

- **The keyboard is grabbed in `remote` mode, which is now the default.** While
  the viewer window is focused, every key — Alt+Tab, the Windows key,
  shortcuts, and your local IME's keys — goes to the host and nothing acts on
  your own machine. This fixes two findings from the control review: local
  Alt+Tab switching your *own* windows instead of the host's, and a local
  Chinese IME doubling/sticking a letter it grabbed at the same moment we
  forwarded it. The host's layout and IME now do all the work. Release the
  grab by clicking another window (the mouse is never grabbed). `local` mode
  (type with your own IME, for a host that lacks your language) grabs only the
  system keys, as before; `auto` is unchanged. It is the standard RustDesk /
  AnyDesk / RDP behavior. Windows only for now; Ctrl+Alt+Del stays reserved
  by Windows. No protocol change (keys already travel as physical codes).

## [0.11.9] - 2026-11-11

### Added

- **The viewer reconnects on its own when the connection drops.** Until now a
  dropped TCP control connection (a route flip, the host's link dying) ended
  the session and closed the window — you had to relaunch. The window now stays
  open, shows "connection lost; reconnecting…", and re-dials the host with a
  backoff until it answers, then resumes the stream. The clipboard, input and
  media slot are kept across the reconnection; only the stream itself restarts.
  A clean end — you close the window, or the host says goodbye — still exits
  without retrying. On by default; `illum view <host> --no-reconnect` turns it
  off (the setting is there for the future UI to expose). No protocol change:
  a reconnect is a fresh session to an already-paired device.

## [0.11.8] - 2026-11-10

### Fixed

- **The viewer now recovers promptly from a lost reference frame.** When a
  frame is lost during congestion, every H.265 delta after it chains off the
  gap and the decoder prints "Could not find ref with POC …" and produces no
  picture — but FFmpeg reports that identically to a normally buffered frame
  (no output, no error), so the viewer kept feeding undecodable deltas and
  never asked for the keyframe that would re-anchor it. It recovered only when
  another path (the 8 s rebuild, or restoring the window) happened to deliver
  a keyframe. The viewer now treats a run of five empty decodes as a lost
  reference and asks for a keyframe at once, turning a flood of decoder errors
  and a slow recovery into a couple of errors and a sub-second one.

## [0.11.7] - 2026-11-10

### Fixed

- **The video no longer dies permanently after a loss spell.** Two causes,
  both fixed:
  - The host only encoded when the screen changed, so a keyframe asked for
    while the host sat idle was never sent — a viewer that had lost its
    reference during a loss spell then waited for ever, video dead while input
    (on TCP) kept working. The host now re-sends the last frame as a keyframe
    when one is owed and the screen is static, so a keyframe request is always
    answerable.
  - The viewer only re-punched and re-asked for a keyframe when video stalled;
    it never rebuilt the path. After eight seconds of a real stall (the host
    reports it is sending, nothing arrives — a route flip, keyframes lost for
    good) the viewer now rebuilds the whole media path by asking the host to
    start the screen again.

## [0.11.6] - 2026-11-04

Protocol 15.

### Added

- **The screen pauses while the viewer is minimized.** Minimizing the window
  sends a new `ScreenPause` message; the host stops capturing, encoding and
  sending the screen, and resumes with a keyframe when the window is restored.
  The connection, input and **audio keep flowing** — so the remote's sound
  still plays with the window down — and nothing actually reconnects. This is
  what RDP does by default. The trigger is a real minimize, not being covered
  by another window.

### Fixed

- **The viewer no longer cries "no video" when the host screen is simply
  static.** It used to treat any silence as a dead path and re-establish once
  a minute. It now uses the host's own per-second `HostStats`: it only says
  "video stalled; reconnecting…" when the host reports it sent video and none
  arrived. A quiet host (nothing changing) stays quiet. Together with the
  pause above, the two causes of the old false alarm are gone.

## [0.11.5] - 2026-11-03

### Fixed

- **Resizing the viewer window past 2048 px stalled the session.** The GPU
  device was created with wgpu's downlevel limits, which cap a texture — and
  so the window surface — at 2048 px. A wider or taller window failed
  `Surface::configure` on every resize tick, no frame reached the screen, and
  the host paused ("no video arriving"). The device now asks for the adapter's
  real limits (16384 on a discrete GPU), and the surface is clamped to that
  maximum as a safety net, so an over-large window (a future multi-monitor
  span) is scaled by the compositor instead of failing.

## [0.11.4] - 2026-11-03

Step 4 of the control review, the last build step: the clipboard. Protocol 14.

### Added

- **Clipboard sync, text, both ways.** A copy on one machine appears on the
  other. It runs when both sides offer it, through the `arboard` crate (one
  code path for Windows, macOS, X11 and Wayland) on the existing `Clipboard`
  channel. A content hash stops echo loops, so the value the peer just set is
  never sent back; the content already on the clipboard at connect is left
  alone. Change detection polls four times a second rather than using
  Windows' `AddClipboardFormatListener`, because the host has no window for
  that message and a poll is the same on every OS. Text over 1 MiB is not
  sent; files come later. Off with `illum --service --no-clipboard` on the
  host and `illum view <host> --no-clipboard` on the viewer.

## [0.11.3] - 2026-11-03

Step 3 of the control review: input delay has a number. Protocol 13.

### Added

- **"input N ms" in the title bar**: the host acknowledges every input event
  as it injects it (moves on the UDP path at most ten times a second), and
  the viewer shows the median round trip of the last second, or the last one
  known while nothing new was typed. It is the time from the key or move
  leaving the viewer to the host having injected it, the input half of what
  "latency" measures for the picture.

## [0.11.2] - 2026-11-03

Step 2 of the control review: mouse moves. Protocol 12.

### Changed

- **Mouse moves travel on the UDP path, newest wins.** On TCP a 1000 Hz
  mouse queued clicks behind its own moves and one lost segment froze the
  pointer until the retransmit. The viewer now sends at most one move per
  4 ms, the latest position, on the media path's own `Input` stream with its
  own sequence numbers; the host drops a move overtaken by a newer one and
  never a key behind it. Before the media path is up, moves go on TCP as
  before.
- **Buttons and wheels carry the pointer's position**, and the host moves
  there before pressing, so a click lands where the mouse was even when the
  move before it is still in flight or lost. A press on the letterbox bars
  does nothing; a release there still lets go.

## [0.11.1] - 2026-11-03

Step 1 of the control review: the key codes on the wire. Protocol 11.

### Changed

- **Keys travel as USB HID usages**, not PC Set-1 scancodes. The scancode was
  the viewer's own platform code, which only works while both ends are
  Windows (macOS has none). The HID usage is the one key code every OS is
  tabulated against, and what Chrome Remote Desktop sends; the viewer maps the
  window library's physical key to it, the host maps it to its injector
  (Windows today; evdev and macOS tables will join with those hosts). A test
  on Windows checks our table against winit's own, key by key. Pause, which
  `SendInput` cannot take as a scancode, now goes as its virtual key, so it
  works for the first time. Keys with no usage (F25 and up, Fn) are not sent.

## [0.11.0] - 2026-11-03

Step 0 of the control review (roadmap M7): the pointer.

### Added

- **`illum view --pointer native|embedded`**, default **native**: your own pointer
  stays visible and wears the host's cursor shape, made into a real cursor of
  the window library from the host's bitmap and hotspot (one code path for
  Windows, macOS, X11 and Wayland; cursors are cached per shape). The OS moves
  it, so it never waits for the viewer's draw or the swap chain, never stutters
  with the frame rate, and never disappears over the dark window or the
  letterbox bars; when the host hides its own pointer, ours shows as the plain
  arrow. This is what RDP, AnyDesk, RustDesk and Parsec do. **Embedded** is
  what every version so far did: the host's cursor drawn inside the picture,
  at the host's size, scaled with it, as a screenshot of the host would show
  it. View-only draws the host's pointer in the picture in both modes.

### Fixed

- In embedded mode the OS pointer was hidden only when the mouse entered the
  window after the host's first cursor shape had arrived; a shape arriving
  while the mouse was already inside left two pointers. The draw now decides.

## [0.10.3] - 2026-11-03

### Fixed

- **The FEC backstop needs the same evidence as every other drop.** The first
  adaptive 0.10.2 session (laptop viewing the desktop over Wi-Fi) ran as
  designed for four minutes, then the backstop took a rung every 30 s window
  for 2–3 % loss that stayed exactly the same from 12.8 Mb/s down to
  4.1 Mb/s and 15 fps: bursty Wi-Fi loss that 30 % parity cannot quite
  finish. The backstop now fires only when the round trip was risen for at
  least half the window (a standing queue, the mark of overload), and a
  further one needs the loss to have at least halved since the last (an
  overload of 2 % vanishes after a 20 % cut). Loss that does not answer to
  the rate is held at the parity ceiling, and the host log says so once a
  window. The harness's bursty-outage scenario showed the same walk (three
  rungs in a minute) and now asserts the ladder stays put.

## [0.10.2] - 2026-11-03

What the first adaptive session on 0.10.1 showed (laptop viewing the desktop
over Wi-Fi): 70 s at the top rung with 10–13 % loss and no picture, because
the fast drop never fired.

### Fixed

- **The round trip the host judges by is the 75th percentile of the last 3 s
  of probes, not the minimum.** On Wi-Fi a path above its capacity delays most
  probes by tens of ms while some still get through at the floor; the minimum
  stayed at 12 ms through 70 s of 12 % loss, so neither the fast drop nor the
  "frozen by overload" rule fired. One probe caught behind a keyframe burst
  still does not move the 75th percentile. The simulator models the same
  statistic and a lost probe's TCP retransmission.
- **Swap chain with two frames in flight** instead of one: a frame arriving
  before the panel had taken the previous one blocked the window for up to a
  refresh, which showed as 10–17 ms of "draw" and tipped frames over the
  50 ms limit, costing a rung for nothing. Mailbox presentation still shows
  the newest frame.
- **The fast drop repeats only while it is working.** A lost clock probe
  comes back by TCP retransmission, so during a loss storm the round trip
  reads as risen as well, and unbounded the fast drop walked the ladder to
  the bottom in seven seconds of a 15 s storm (harness). The tell is what gets
  through when the host sends less: a saturated link keeps delivering the
  same amount, a storm delivers a fixed share. Within 10 s of a fast drop the
  next one needs the received count to have held up; a 15 s storm now costs
  three rungs and the top is back within 90 s, while a 20 Mb/s start still
  gets under a 12 Mb/s link in three seconds.
- **The 10 s drop rule needs the second that completes the count to be bad
  itself.** Four 🔴 seconds followed by six 🟢 ones are a path that has
  recovered; the rule dropped a rung into six seconds of green evidence five
  seconds after a storm ended.
- The simulator's clock probes are sampled on the wall clock, four a second,
  like the viewer's, instead of at frame boundaries, where the host's own
  pacer backlog read as path queueing; a packet counts as received in the
  second it arrives, not the second it was sent; and a 30 fps tick no longer
  sends the same frame twice on a floating-point boundary, which had shown as
  33 frames a second and 46–63 ms of latency at that rung on a clean link.
- **Connecting took five seconds on the laptop**: the viewer probed every
  encoder (two seconds of failing to open NVENC, AMF and QSV, which a viewer
  never needs) and then the GPU decoders (three seconds on a two-GPU laptop)
  before every connection. The viewer now skips the encoder probe, and the GPU
  decoder answer is kept in the home folder for a week.

## [0.10.1] - 2026-11-03

### Changed

- Documentation brought in line with 0.10.0: the ladder as built (7 rungs from
  the default cap), the keyframe interval, the viewer notices under
  diagnostics, the results document pointing at the rules it produced, and the
  parity flags described as measurement-only in `illum view --help`.

## [0.10.0] - 2026-11-03

The adaptation tuned from the measurement runs (docs/testing/results-2026-11-02.md),
and the settings split into "fixed at connect" and "limits for the adaptation"
(architecture §5.8).

### Added

- **Fast drop for congestion**: a second with loss ≥ 2 % and a round trip at least
  1.5× (and 10 ms above) its floor drops one rung at once, at most one per second
  while it lasts. At a rung above the link this signature showed inside the first
  second; the 10 s window took thirty seconds for three rungs.
- The viewer's `MediaReport` carries two round trips (protocol 10): the smallest
  of the last 3 s of clock probes, so a queue that never drains shows while our
  own bursts and keyframes do not, and the smallest of the last minute as the
  floor, which the probes before the stream started already set. Probes now go
  four times a second.
- **Notices in the title**: "waiting for a keyframe" 2 s after asking for one, and
  "re-establishing the media path" when no media datagram has arrived for 3 s
  while the control channel is alive (the viewer then punches fast again and asks
  for a keyframe). Both went unnoticed for 9–25 s in the runs.
- `view --allow-scale`: the ladder's resolution rungs, off by default.

### Changed

- **Lost frames count as 🔴 only when the path's timing rose** too. With a flat
  round trip they are random loss, parity's business, and the second is ⚪;
  loss-only 🔴 seconds had walked the ladder down for nothing.
- **Raises are gated on loss over the last 30 s**, the same window as the FEC
  controller, instead of 10 s; a backstop drop was being undone 15 s later.
- **Keyframes at most two per second** (was four): four a second fed the collapse
  above the link; one a second, tried first, left the harness's bursty link
  showing under half its frames.
- **Default frame-rate cap 30, settable up to 120**; the frame-rate rungs are
  those below the cap (60/45/30/20/15).
- **Resolution rungs only when allowed**: the host scales on the CPU, which
  measured as 10–15 ms of latency and a 45–53 fps cap.
- Parity's range is no longer a user setting; the controller keeps it between
  5 and 30 %.
- Harness: the round trip is modelled (base delay plus the path's queue); new
  scenarios "congestion at the top, 12 Mb/s" (under the link within 8 s, holds)
  and "15 s at 75 % loss with flat timing" (ladder untouched, picture back within
  5 s); random loss must never move the ladder at all.

## [0.9.8] - 2026-11-02

### Added

- `docs/testing/results-2026-11-02.md`: the four measurement runs organised —
  the pipeline stage by stage in both directions, the link's ceiling, what
  each setting costs, the faults the runs found, what is still open, and what
  the data says about the controllers.

## [0.9.7] - 2026-11-02

### Fixed

- The report's unit test, left failing by 0.9.6 (its synthetic seconds had
  no step time).

## [0.9.6] - 2026-11-02

### Changed

- `illum report` also leaves out the first 3 s of each step: the opening
  keyframe and the decoder and GPU warming up showed as a 250–500 ms stall in
  whichever phase a step happened to start, which the 2K run made obvious.
  The table header now says so, and that the still phase's ~1000 ms stall is
  the content's own one change per second.

## [0.9.5] - 2026-11-02

### Fixed

- A pts that QSV rounded to its 90 kHz clock (up to 11 µs off) is snapped
  back to the capture time of the frame it belongs to; the encoder lag test
  on the laptop failed on that rounding alone, with every frame otherwise
  coming back in its own call.

## [0.9.4] - 2026-11-02

What the second reverse run (desktop viewing the laptop, now with H.265
flowing) showed about the laptop's `hevc_qsv`.

### Fixed

- **QSV handed every frame back four frames late** (its default `async_depth`):
  133 ms of latency at 30 fps, 4 s at 1 fps in the still phase. Now
  `async_depth=1`.
- **QSV and AMF never produced a key frame on request**: a forced picture
  came out as a plain I picture, not flagged, so after the first lost frame
  the viewer waited for a keyframe until the session ended while the host
  kept sending at full rate (25 s freezes in the run). Now `forced_idr=1` on
  both, and AMF runs with `usage=ultralowlatency`.

## [0.9.3] - 2026-11-02

What the reverse measurement run (desktop viewing the laptop) showed: seven
of ten steps recorded nothing, because the laptop's `hevc_qsv` was fed BGRA
and encoded **4:4:4** H.265, which the desktop's D3D11VA refused and the
software decoder returned as YUV444P, which the viewer rejected. Until 0.8.0
swscale hid this by converting every decoded frame to BGRA.

### Fixed

- **Encoders other than NVENC are fed NV12 (or 4:2:0 planar), never BGRA.**
  NVENC converts BGRA to 4:2:0 on the GPU; QSV given BGRA encodes 4:4:4.
- **A decoder that returns something other than 8-bit 4:2:0** (4:4:4, 4:2:2,
  10-bit) is converted to 4:2:0 once on the CPU, with one warning, instead of
  failing every frame.
- **A GPU decoder that fails on a stream hands over to the CPU decoder** once
  and asks for a keyframe, instead of asking for a keyframe for ever.

## [0.9.2] - 2026-11-02

What the first measurement run (laptop viewing the desktop, 24 steps) showed
about the measuring itself.

### Fixed

- **The viewer no longer holds the state lock while drawing.** The window took
  the lock, converted, scaled and presented, and only then let go; the receive
  thread waited behind every draw. On the GPU path that was a few ms, on the
  CPU path 70–90 ms per frame, so the receive thread fell further and further
  behind until "latency" read 30 s. The window now copies what it needs,
  draws without the lock, and takes it again only to record the result. The
  CPU-path rows of the first run measured this bug, not the path.
- **The CPU YUV → BGRA conversion is integer fixed-point** instead of
  floating point per pixel; the colours are the same to within a unit.
- **The still phase is still.** The workload window re-presented an unchanged
  picture 60 times a second, and the compositor handed each one to the
  capture as a new frame, so the host sent 58 fps of identical pixels. It now
  presents only when the picture changed: once a second while still.

### Added

- `scripts/measure-plan-short.csv`: the 8 steps at 6 Mb/s (both codecs, all
  four decode/render combinations), for re-checking a viewer machine.

## [0.9.1] - 2026-11-02

### Added

- `illum workload --screen N` fills monitor N instead of the first, so the
  host's own desktop stays usable during a run; `scripts/measure.ps1 -Screen N`
  makes the viewer stream that display.

## [0.9.0] - 2026-11-02

Measurement runs: the simulated-link harness, on the real link. The ladder
and parity numbers get tuned from these tables rather than from how a
session felt (docs/testing/measure.md).

### Added

- **`illum view --fixed`**: the host streams at exactly `--max-bitrate`,
  `--max-fps`, `--fec-min` and the new `--scale` percent for the whole
  session; the controllers are not consulted (`ScreenStart.fixed`,
  `scale_percent`; protocol 9). `--codec av1|hevc|tiles` narrows what the
  viewer offers, `--duration N` closes the window after N seconds, and
  `--report file.json --label name` writes every second of the session:
  frames, bytes, keyframes, per-frame latency / decode / draw / encode
  samples, longest stall, rtt, loss, repairs, frames lost, plus the host's own
  per-second numbers.
- **`HostStats`** (host → viewer, once a second): frames out of capture,
  skipped by the frame-rate cap, sent (and bytes), keyframes (and bytes),
  encoder time. The report can tell a still screen from a stalled link.
- **`illum workload`**: a full-screen window for the host side of a run, on
  the wall clock: 20 s still (one change per second), 20 s scrolling fine
  text, 20 s full motion, with a seven-segment clock in the corner (seconds
  while still, milliseconds otherwise, for slow-motion camera measurements).
- **`illum report <folder>`**: one Markdown table from a folder of report
  files, one row per step and phase (the first two seconds of each phase
  left out): fps, Mb/s, latency p50 / p95 / max, stall, decode, draw, encode,
  rtt, loss, frames lost, keyframes, host taken / capped / sent.
- **`scripts/measure.ps1`** with `scripts/measure-plan.csv`: 24 steps of
  60 s (bitrate sweeps per codec, decode and render variants, frame rate,
  parity and scale variants, tiles), one fresh session per step, then the
  table. Run it from each machine for the two directions.
- Tests: the workload phases and content, the recorder, the report
  aggregation and JSON round trip, and an in-process fixed session that holds
  its settings, scales to 50 % and records seconds with host stats.

### Fixed

- The loopback tests no longer fail now and then with WSAEACCES (10013) when
  binding the media port. With port 0, Windows picked the TCP port
  sequentially, and once it was inside a UDP-reserved block (Hyper-V, WSL)
  every retry landed in the same block. The listener now draws the shared
  port at random from the dynamic range and keeps a refused port's TCP
  listener until a pair works (45 binds × 15 runs without a failure).

## [0.8.0] - 2026-11-02

M8, the viewer half: the picture no longer touches the viewer's CPU. Built
with the three operating systems in mind — FFmpeg's hardware paths and `wgpu`
carry the OS differences; nothing here is written against a Windows API.

### Added

- **GPU decoding** (`illum-ffmpeg`): AV1 and H.265 through FFmpeg's hardware
  decode paths — D3D11VA (then DXVA2) on Windows, VAAPI / CUDA / VDPAU on
  Linux, VideoToolbox on macOS, one list — with each surface copied down once
  as NV12. On a two-GPU machine the second and third adapter get a try. What
  the GPU can decode is found by decoding one embedded 256×144 keyframe per
  codec (`crates/illum-ffmpeg/probe/`), the same check on every vendor and OS;
  `illum codecs` prints it under "Decoders (GPU)". A viewer with
  `--software-decode`, or whose GPU has no path for the codec, decodes on the
  CPU as before; a probed decoder that will not open for a session falls back
  to the CPU and says so in the log.
- **GPU presentation** (`illum-ui::gpu`, `wgpu`): the decoder's planes are
  uploaded as they are and one shader converts (BT.601 / BT.709, limited /
  full range, from the picture's own description), scales and blends the
  cursor. The swap chain uses the newest-frame-wins mode (Mailbox, else
  Immediate, else Fifo) with a frame latency of 1, so presenting never blocks
  and a late frame never queues behind an older one. Fit and Original modes
  and the locally drawn cursor behave as before. `--software-render` forces
  the CPU path, which also remains the fallback on a machine with no usable
  GPU API.
- Tests: offscreen renders of the colour pattern from I420 and NV12 planes in
  both matrices and both ranges with unaligned strides, half-size scaling,
  cursor blending at the hotspot; GPU decode round trips and lag checks for
  both codecs (they skip with a note where there is no GPU).

### Changed

- Decoders hand the window their native 4:2:0 planes (`Picture::Yuv`) instead
  of converting to BGRA with swscale; the tile codec returns `Picture::Bgra`.
  The encoder now declares BT.601 limited range in the stream, which is what
  its BGRA → 4:2:0 conversion produces, so the viewer converts back the same
  way.
- AV1 is offered by a viewer whose GPU decodes it; the rule from 0.7.2 now has
  something to find, so this PC ↔ laptop goes back to AV1 when the laptop's
  GPU decodes it.

## [0.7.3] - 2026-11-02

What an hour of nobody watching taught.

### Fixed

- **The ladder no longer ratchets down behind a hidden window.** With the
  viewer minimised (or its display off) nothing is presented, so almost no
  second is graded; the laptop's Wi-Fi hiccups for 3 s every five minutes,
  those seconds graded 🔴, and "3 of the last 10 graded seconds" was met with
  evidence spread over twenty minutes — one rung lost per quarter hour. Two
  rules: the 10 graded seconds of a drop must lie within 30 s (a raise's
  within four times its window), and "frozen by loss" now looks at frames
  **decoded**, not shown — a hidden window decodes everything and presents
  nothing, which is no evidence about the link at all.
- **The host stays awake while it streams** (system and display): an idle
  hour put the host to sleep, which took the capture and the connection with
  it. Roadmap M6's "wake lock", pulled forward.
- An unexpected `AcquireNextFrame` error (display turned off, device state)
  recreates the duplication instead of ending capture; capture gives up only
  after 30 s of continuous failure.
- The host logs once when the viewer decodes but does not present.

### Changed

- Protocol version 8: `MediaReport` carries `decoded`.

## [0.7.2] - 2026-10-30

The codec rule enforced as written, and the numbers to judge the viewer by.

### Changed

- **A viewer offers AV1 only when it decodes AV1 in hardware.** Since v0.5.1
  the viewer advertised `dec:av1` on the strength of the software decoder
  (`dav1d`), so the laptop received AV1 and spent ~20 ms per 1080p frame
  decoding it on the CPU — the main part of the lag next to RustDesk. The
  rule (§5.3) was always "AV1 if both sides can use it in hardware"; until the
  GPU decode path lands (M8) every viewer therefore gets H.265, whose software
  decoder costs half as much, and AV1 comes back the day the hardware path
  exists.
- New viewer setting **hardware decode**, default on: `illum view
  --software-decode` turns it off (CPU decoders only, so never AV1).
- NVENC encodes AV1 in 4 tile columns, so a software decoder can use several
  cores without adding a frame of delay (~2 % more bits).
- The title bar and viewer log now show `decode x ms` and `draw y ms` next to
  `encode`, so the viewer's own cost is visible.

### Verified

- `cargo test`: 136 tests pass (the end-to-end codec test now expects what
  the rule yields on the test machine); `cargo clippy` is clean.

## [0.7.1] - 2026-10-30

What the first live run of the adaptation taught (this PC → laptop over
Wi-Fi, a two-hour session).

### Fixed

- **Clock drift blinded the latency controller.** The viewer's estimate of
  the host clock came from the single fastest probe of the whole session; the
  two machines' clocks drift apart by tens of microseconds per second, and
  after two hours the estimate was ~80 ms stale, so the title showed
  `latency −37 ms`, every frame counted as "within 50 ms" and the ladder never
  moved. The offset now comes from the fastest probe of the **last minute**.
- **Keyframes get at least 30 % parity.** A keyframe is ~180 packets; with
  5–10 % parity on a Wi-Fi burst it mostly died, the viewer waited, the retry
  died too — stalls of 1–20 s (the "laggy" feeling). Keyframe requests are
  also honoured every 250 ms instead of 500 ms.
- The "frozen by loss" grade now triggers at **2** lost frames in a second
  with almost nothing shown (was 8): during a stall only the two keyframe
  attempts per second die while the small frames in between arrive.
- The frame-rate cap skipped frames captured a microsecond early; at 60 fps
  that cost up to a third of the frames.
- Capture times now travel through the encoder as `pts` and come back with
  each packet, so a codec that returned frames late could not mislabel them
  (NVENC and the decoders were tested and do not lag; the guard stays).

### Verified

- `cargo test`: 136 tests pass, including the new ones: clock sync follows
  a 12 ppm drift over a simulated two hours; AV1 and H.265 encoders and
  decoders return the frame they were given (checked by the test pattern's
  moving bar at 1080p). `cargo clippy` is clean.

## [0.7.0] - 2026-10-30

Audio, the Sunshine way (architecture §8). The host's sound reaches the
viewer; nothing adapts, nothing waits.

### Added

- **Audio capture and playback** (`illum-win`): WASAPI loopback of the
  default output on the host, WASAPI playback on the viewer, both asked for
  48 kHz stereo 16-bit with the engine converting.
- **Opus** through FFmpeg's `libopus` (`illum-ffmpeg::opus`): 5 ms frames,
  ~80 kb/s, low-delay application, one packet per frame.
- **Audio over the UDP media path** (`illum_media::audio`): an 8-byte header,
  every 4 frames protected by 2 Reed–Solomon parity packets (a 20 ms group;
  any 4 of 6 rebuild the rest), data packets sent the moment they are
  encoded. Separate stream, separate sequence numbers, same keys.
- **Fixed 40 ms jitter buffer** on the viewer; a missing frame is 5 ms of
  silence; 100 ms of nothing means the host is quiet and the buffer
  re-primes when sound returns; a backlog is skipped, never played late.
- `illum --service --no-audio` ("send audio: off": nothing is captured or
  sent); `illum view --mute` (don't ask for sound). The test pattern host
  hums a 440 Hz tone. The title bar shows `audio` while frames arrive.

### Changed

- Protocol version 7: `ScreenStart` carries `audio`; capability `audio`.

### Verified

- `cargo test`: 131 tests pass — audio packets and parity groups (two lost
  of six rebuilt, three lost handled), the jitter buffer (priming, holes,
  pauses, backlog), an Opus round trip through the real encoder and decoder
  (level within 2 dB, ~80 kb/s), and end to end over loopback UDP: a viewer
  that asks gets ≥ 100 frames in half a second, a muted one gets none.
  `cargo clippy` is clean.
- Not yet heard live between the two machines.

## [0.6.1] - 2026-10-30

Documentation only.

### Changed

- `docs/architecture/adaptation.d2` redrawn as the per-second adaptation
  cycle as built in v0.6.0: the viewer's measurement, the report, the grade
  (with the freeze-by-loss rule), the latency controller with back-off, the
  FEC controller with its zones and backstop, what gets applied, and the
  `Quality` message back to the viewer.

## [0.6.0] - 2026-10-30

Milestone M5: adaptation. The host now finds the quality the link can carry
instead of sending the cap and hoping.

### Added

- **The ladder and the latency controller** (architecture §5.5). Every
  second the viewer reports how many frames it showed and how many arrived
  within the latency limit (default 50 ms). The host grades the second
  🟢/🟡/🔴/⚪ and walks a ladder — bitrate × 0.8 steps, then 60/45/30/20/15 fps,
  then 75 % and 50 % resolution, bottom ~1 Mb/s — one rung down when 🔴 fills
  30 % of the last 10 graded seconds, one rung up when 🟢 fills 80 % of the
  last 15, never while the path is losing packets. Starts at the top.
- **The FEC controller** (§5.6): packet loss before repair over 30 s windows
  moves parity +5 points per 🔴 window (≥ 0.5 %) up to the ceiling, −5 per 🟢
  window (< 0.1 %) after two in a row down to the floor; defaults 5–30 %.
  Parity comes out of the rung's budget. At the ceiling with frames still
  dying, it asks the ladder for one rung down.
- **Simulated link** (`illum_media::sim`, §5.7) and six scored scenarios in
  `crates/illum-media/tests/harness.rs`: utilisation, flaps, recovery time,
  false drops, seconds over the limit.
- Viewer settings as flags: `--latency-limit <ms>`, `--max-fps`,
  `--fec-min`, `--fec-max` (with the existing `--max-bitrate`).
- The host tells the viewer its rung and parity (`Quality`); the title bar
  shows `rung 3/11: 12.8 Mb/s 60 fps, parity 10 %`, the host log says why
  each change happened.

### Changed

- Three rules that the harness forced, all recorded in §5.5: a second with
  almost nothing shown is ⚪ only if nothing was lost — if at least half the
  grading threshold of frames were *lost*, it is 🔴 (an oversubscribed path
  otherwise left the controller blind for minutes); a raise that is undone
  within 60 s backs off (the green run needed doubles, 15 → 30 → 60 → 120 s),
  which stops the hunting between the rung that fits and the one above; the
  backstop fires only while frames are still being lost.
- The pacer's burst now holds at least two frames (was a fixed 50 ms), and the
  encoder gets `rung × 100 / (100 + parity) × 90 %` instead of a fixed 85 %:
  frames leave at link speed, and parity really is budget-neutral.
- Encoders are reopened when the rung changes frame rate or resolution
  (hardware encoders take bitrate changes live; software ones are reopened).
- Protocol version 6: `ScreenStart` carries the settings, `MediaReport` the
  shown/within counts, and `Quality` is new.

### Verified

- `cargo test`: 122 tests pass, including the six harness scenarios and the
  end-to-end run that checks the host's first `Quality` (top rung, 5 %
  parity). `cargo clippy` is clean.
- Not yet measured live between the two machines.

## [0.5.8] - 2026-10-28

### Fixed

- **Stream dying for good after a lost frame.** The viewer asks for a
  keyframe when a frame is lost; the host honours that at most twice a
  second — and a request arriving inside that window was *dropped*, leaving
  the viewer skipping every frame until the end of time. The request now stays
  pending until it is served, and the viewer repeats it once a second while
  non-keyframes keep arriving (which also covers a keyframe lost in full).
- Media sockets ask for 4 MB kernel buffers instead of Windows' 64 KB
  default, so a keyframe burst no longer overflows the receive buffer.

### Added

- **Log files.** Each role writes `<home>\logs\<role>.log` at debug level
  (`service.log`, `viewer.log`; home is `%ProgramData%\Illumination`), next
  to the console output. The path is printed at start.

## [0.5.7] - 2026-10-28

### Added

- **GDI capture fallback.** When Desktop Duplication is not available from
  the host process (on the laptop, Windows had bound illum to the discrete
  GPU, which cannot duplicate the panel), capture switches to GDI the way
  RustDesk does: `BitBlt` at up to ~60 fps, unchanged frames dropped, cursor
  from `GetCursorInfo`. Slower than duplication, but it streams without any
  Windows setting. The host log says which method is in use.

### Changed

- The "set Power saving in Windows graphics settings" advice is gone from the
  error path; it is a log hint only.

## [0.5.6] - 2026-10-28

### Fixed

- The viewer's "no video within 5 s" message no longer replaces an error the
  host already reported (on the laptop it hid the real capture failure behind
  a firewall guess), and it is worded as the guess it is.

## [0.5.5] - 2026-10-28

### Fixed

- **Hosting from a two-GPU laptop.** On the laptop, `DuplicateOutput` failed
  with `DXGI_ERROR_UNSUPPORTED` and the host retried forever behind a black
  viewer: the NVIDIA layer lists the panel under the discrete GPU when the
  program runs on it, but only the GPU that drives the panel can duplicate
  it. Capture now keeps every adapter that lists the display and moves to the
  next one when one refuses; if all refuse, the viewer gets the reason and
  the Windows graphics-settings fix instead of a black window.

## [0.5.4] - 2026-10-28

### Fixed

- `scripts/get-libclang.ps1` falls back to 7-Zip when Windows' `tar.exe`
  exists but cannot unpack `.xz` (older builds shell out to a missing `xz`
  program), and `-Use7Zip` forces that path. A finished download is reused
  across attempts.

## [0.5.3] - 2026-10-28

### Fixed

- `scripts/get-libclang.ps1` fetches LLVM's `.tar.xz` instead of `.tar.zst`
  and unpacks it with Windows' own `tar.exe` (7-Zip only as a fallback): the
  `.zst` asset needs 7-Zip 23.01+, which the laptop didn't have. Both fetch
  scripts also turn off PowerShell's progress bar, which made the downloads
  several times slower, and the libclang script reuses a finished download.

## [0.5.2] - 2026-10-28

### Removed

- Software AV1 (`libsvtav1`) is no longer an encoder candidate. The rule is
  AV1 in hardware, otherwise H.265; a machine without a hardware AV1 encoder
  goes straight to H.265 (hardware, or `libx265` as the last resort).

## [0.5.1] - 2026-10-22

Milestone M4, second slice: the video codec and parity FEC. Lossless tiles
remain only as the fallback.

### Added

- **AV1 / H.265 video through FFmpeg** (new crate `illum-ffmpeg`, FFmpeg 8.1
  prebuilt, fetched once by `scripts/get-ffmpeg.ps1`; `scripts/get-libclang.ps1`
  fetches what the bindings need to build). At start each side probes which
  encoders actually open here (NVENC, AMF, QSV, then software) and lists them
  in its hello as `enc:av1`, `enc:hevc`, `dec:av1`, `dec:hevc`. The host picks
  **AV1 if it can encode it and the viewer decode it, otherwise H.265,
  otherwise tiles**; the choice is per direction, 4:2:0, and shown in the
  title bar and the log. Encoders run with no B-frames, no lookahead and a
  100 ms rate buffer; decoders with slice threading only, so a packet yields
  its picture at once.
- **Parity FEC.** Every frame's packets carry Reed–Solomon parity (blocks of
  ≤ 128 packets, **5 %** of each block, at least one packet), so the viewer
  rebuilds lost packets without waiting or a round trip. Repaired frames are
  counted (`MediaReport.repaired`, "N repaired" in the title). The 5–30 %
  controller is M5.
- `illum codecs`: what this machine can encode and decode, with FFmpeg's
  reason for each hardware encoder that would not open.

### Changed

- Protocol version 5: a new packet header (codec id, block and shard fields,
  30 bytes) and the `repaired` field in the loss report.
- Reference invalidation is served with a **keyframe** for now: FFmpeg gives
  no per-frame reference control for the hardware encoders (architecture §13).
- FFmpeg 8.1 rather than 9.0, whose NVENC needs NVIDIA driver 610+ and would
  otherwise fall back to software unnoticed; 8.1 needs 570+.
- `illum-ffmpeg`'s build copies the FFmpeg DLLs next to every executable that
  links it (so tests run too).

### Verified

- `cargo test`: 104 tests pass. New: Reed–Solomon parity and rebuild;
  block-based packetisation with repair, out-of-order and stalled frames;
  the tile codec behind the common codec interface; AV1 and H.265 round trips
  of the colour pattern through the real encoders (`av1_nvenc`, `hevc_nvenc`
  on this PC) and decoders, every patch within a colour distance of 12;
  end to end over loopback UDP: the host picks AV1 for a viewer that
  advertises `dec:av1`, colours survive; a viewer without decoders gets
  tiles, bit-exact; through a 1 % lossy relay frames are **repaired by
  parity** (more repaired than lost) and the pattern stays intact.
  `cargo clippy` is clean.
- Not yet run live between this PC and the laptop.

## [0.5.0] - 2026-10-22

Milestone M4, first slice: the media transport. Video now travels over UDP
and is never resent; control and input stay on TLS over TCP.

### Added

- **UDP media path.** The host binds UDP on the same port number as TCP
  (default now **28326** for both; `--port` moves both). The viewer sends
  small "punch" datagrams so the host learns its address, then video flows
  host → viewer. Every datagram is encrypted and authenticated with keys
  **exported from the TLS session** (RFC 5705), so there is no second
  handshake; the host finds the session by an id in the datagram header and
  rejects replays and forgeries.
- **Packetisation and reassembly.** Frames are split into ≤ 1200-byte
  datagrams (fits Tailscale's MTU). A frame is shown only when every packet
  has arrived; a frame whose packets stop, or that a newer frame overtakes,
  is reported lost and the viewer asks for a keyframe (the host honours that
  at most twice a second). Until the keyframe arrives the viewer keeps showing
  the last good picture.
- **Pacing by bitrate.** A token bucket paces datagrams to a cap
  (`view --max-bitrate <Mb/s>`, default 20, capped by the host at 20). The host
  encodes only the newest captured frame once the previous one has left, so
  nothing queues up. The old "two frames in flight" window is gone;
  acknowledgements are feedback only.
- **Loss feedback.** The viewer reports datagrams received and lost (counted
  by sequence gaps, before any repair) and frames lost, once a second; the
  title bar shows `loss x%`.
- **Blocked-path detection.** If no punch reaches the host within 5 s, or no
  video reaches the viewer, each side says so in plain words instead of
  hanging.

### Changed

- Protocol version 4; a `udp-media` capability. Hosts and viewers from
  v0.4.x can't connect to v0.5.0 (the hello refuses the mismatch cleanly).
- Video no longer travels on the TCP channel at all. Cursor updates still do.

### Verified

- `cargo test`: 89 tests pass, including: datagram seal/open/replay/forgery
  tests; both sides derive the same media keys from a real TLS handshake;
  packetisation with out-of-order and stalled frames; the pacer's average
  rate; and end-to-end runs over loopback UDP (bit-exact colour pattern,
  recovery through a 1% lossy relay, a blocked path reported within 5 s).
  `cargo clippy` is clean.
- Live on this PC over its Tailscale address: test pattern at **59 fps,
  9 ms capture-to-screen, 0.0% loss, RTT 0.3 ms**. Real screen capped at
  6 Mb/s: the pacer held ~6 Mb/s with **RTT flat at 0.2 ms** (no queue);
  the picture ran at 2–4 fps because lossless tiles of a full-screen change
  are about 2 MB per frame, which is what the hardware codec (v0.5.1) is for.
- Not yet run from the laptop.

## [0.4.4] - 2026-10-22

Documentation only. Two decisions that close the open questions before M4.

### Changed

- **Codec library: FFmpeg**, for all platforms from the start, with a trimmed
  build bundled in the installer.
- **Chroma: 4:2:0 in v1** for both AV1 and H.265. A 4:4:4 mode on H.265, for
  sharper coloured text at roughly 1.5–2× the bitrate, is noted as future work
  to try once 4:2:0 is stable (roadmap M8).

## [0.4.3] - 2026-10-22

Documentation only: the design decisions from the bandwidth discussion, after
the first two-machine test. No code changes.

### Changed

- **Architecture:** media (video and audio) moves to **UDP and is never
  resent**; control and input stay on TLS over TCP; one port number for both,
  default **28326** (code still uses 7171 until the media path lands). Codec:
  **AV1 if both sides can, otherwise H.265**, chosen per direction. Loss:
  **parity FEC over the whole frame** (5–30%, adaptive, budget-neutral) plus
  reference invalidation. A full **adaptation design**: a latency controller
  on a user-set limit (50 ms) with per-second grades, interval rules and a
  one-rung-at-a-time ladder, and a separate slow FEC controller, with three
  coordination rules. Audio the Sunshine way: Opus 5 ms frames, fixed 4+2
  parity groups, ~40 ms buffer. User settings, the tunable numbers and the
  simulated-link test harness are written down. Ideas discussed and not
  adopted are listed so they aren't re-proposed.
- **Roadmap:** re-planned. M4 is now the media transport and codec, M5 the
  adaptation and harness; the service, desktop features, quality and release
  milestones became M6–M9. M1–M3 checks updated with the laptop results.
- **Lessons:** AnyDesk's bandwidth behaviour and connect timeline from the
  two sessions; RustDesk's adaptation as documented in its `video_qos.rs`;
  Sunshine/Moonlight's FEC, audio, connection-status thresholds, bitrate
  table and pacer, from their source; the two-machine test numbers; the
  laptop's codec support; the port check.
- Two new diagrams: `media-transport.d2` and `adaptation.d2`;
  `milestones.d2` updated.

## [0.4.2] - 2026-10-19

Fixes from the first two-machine test.

### Fixed

- **Small text looked broken in the viewer.** Shrinking the host screen to fit
  the window dropped whole pixel rows and columns, so thin strokes vanished in
  places. Each window pixel now averages the host pixels it covers.
- **Alt+Tab and the Win key stayed on the controlling machine.** The keyboard
  hook was registered without a module handle, which Windows can refuse. It now
  passes the program's module, switches on even if the window already has focus
  when it opens, and prints a clear note if it still fails.
- **Closing the viewer panicked** ("there is no reactor running"). The
  shutdown timer was created outside the async runtime.
- **The laptop console filled with stats and colour codes.** The once-a-second
  stats line is now only shown with `-v` (the window title still has it), and
  colour codes are off on Windows consoles, which printed them raw.

### Added

- **Original-size view:** 1:1 pixels, and the visible part follows the mouse
  when the host screen is larger than the window. **Ctrl+Alt+Shift+F2**
  switches between fit and original size; the hotkey is never sent to the host.

### Changed

- `Cargo.toml` names the real repository.

### Verified

- `cargo test`: 74 tests pass. New tests check that averaging keeps a
  one-pixel line instead of dropping it, that the original-size view pans and
  maps the mouse back correctly, and (with `-- --ignored`) that the keyboard
  hook installs. `cargo clippy` is clean.
- Live: when the host goes away, the viewer now exits with a plain error
  instead of a panic, and the console shows no stats spam or colour codes.

## [0.4.1] - 2026-10-19

### Fixed

- README: the "Try it" steps now run in PowerShell. PowerShell needs `.\illum.exe`
  to start a program from the current folder; plain `illum` fails. The steps
  also show how to clone and build on each machine.

## [0.4.0] - 2026-10-19

Milestone M3, input: you can control the host's keyboard and mouse from the
viewer.

### Added

- **Keyboard:**
  - keys go by position (scancode), so the host's layout and IME apply
  - three modes, as in AnyDesk, with `--keyboard remote|local|auto`; Auto
    compares the two machines' keyboard layouts, which the host reports
    whenever its layout changes
  - in Local mode, characters and this machine's IME result are sent as text
    and typed on the host as Unicode
- **No stuck keys and no extra Enters:**
  - only the host repeats keys (after 500 ms, every 40 ms); the controller
    drops its own repeats
  - while keys are held, the controller sends heartbeats
  - the host stops repeating after 300 ms of silence and releases everything
    after 1 s, when the viewer loses focus, or when the session ends
  - duplicate presses and old sequence numbers are ignored
- **Mouse:** absolute positions on the shown display, buttons (including
  back/forward) and wheel. The host is DPI-aware, so positions match the
  captured pixels.
- **Cursor:** while controlling, the host's cursor shape is drawn at your own
  mouse position, so it never lags.
- **System keys:** Win, Alt+Tab, Alt+Esc, Ctrl+Esc and Alt+F4 go to the host
  while the viewer has focus (low-level keyboard hook).
- **View-only:** `--view-only`. The viewer also opens view-only on its own if
  the host doesn't offer input.

### Changed

- Protocol version 3: input channel messages, a keyboard-layout message, and an
  `input` capability.
- Hosts advertise only the capabilities they have. A test-pattern host offers
  no input.

### Verified

- `cargo test`: 71 tests pass, including:
  - the input rules replayed with a fake clock (a 3 s network drop while
    holding Enter gives no extra Enters and no stuck key)
  - an end-to-end test where a real connection drops while Enter is held and
    the host releases it
- `cargo clippy` is clean.
- Live on this PC: the viewer opens view-only against a test-pattern host, and
  real-screen streaming still works.
- Real keyboard and mouse injection is not yet tested by hand. It needs two
  machines, because controlling your own PC loops input back into the viewer.

## [0.3.0] - 2026-10-19

Milestone M2, first pixels: you can see another machine's screen live in a
window. Input comes in M3.

### Added

- **`illum view <host>`:** opens a window with the host's screen, shown 1:1 or
  scaled down to fit. The title bar shows frame rate, bandwidth, latency,
  encode time and round-trip time. `--screen N` picks another display.
- **Screen capture on Windows** with DXGI Desktop Duplication. Only the BGRA8
  desktop format is accepted, and capture is recreated after a mode change.
  `illum displays` lists monitors, primary first.
- **Lossless tile codec:**
  - 64×64 tiles; only tiles that changed are sent, each LZ4-compressed
  - colours arrive bit-exact
  - about 3 ms encode at 1080p
  - like AnyDesk's segmented DeskRT, a screen codec rather than a video codec
- **One pixel format** (BGRA8) end to end, translated for display in exactly
  one function. Tests pin the channel order, so a red↔blue swap fails the build.
- **Cursor sent separately** from the video (position and shape, including
  colour, masked-colour and monochrome cursors) and drawn by the viewer.
- **Frame pacing:** the viewer acknowledges each frame it shows, and the host
  keeps at most two frames in flight, always sending the newest screen, so
  delays never pile up.
- **Clock sync** over the control channel to measure capture-to-screen latency.
  The probes also keep a session on a still screen from timing out.
- **Colour test pattern:** `illum --service --test-pattern` streams eight
  colour patches, a grey ramp and a moving bar instead of the screen.
- New crates: `illum-media` (portable media), `illum-win` (Windows capture) and
  `illum-ui` (viewer window).

### Changed

- Protocol version 2: screen start/stop, screen errors, frame
  acknowledgements, clock probes, and a `screen` capability.
- Host logs show the full cause of an error, not just "network error".

### Verified

- `cargo test`: 53 tests pass, including an end-to-end test over TLS that
  checks the colour pattern arrives bit-exact and that the host stops after two
  unacknowledged frames. The real-capture test (`-- --ignored`) passes on this
  PC. `cargo clippy` is clean.
- On this PC over its Tailscale address: the test pattern ran at 59 fps,
  0.3 Mb/s, about 5 ms capture-to-screen. The real desktop streamed live (15–36
  fps in the worst case, where the viewer shows itself and every frame changes
  completely).
- Not yet run from the laptop, and the cursor is not yet checked by eye.

## [0.2.0] - 2026-10-19

Milestone M1, connect and trust: two machines connect directly, check each
other's keys and measure round-trip time. There is no screen yet.

### Added

- **Hosting:** `illum --service` listens on this machine's **Tailscale addresses
  only**, port 7171. Other addresses are opt-in with `--bind <IP>`. There is no
  UPnP.
- **Connecting:** `illum connect <host>` dials an IP address or MagicDNS name
  (port optional) and prints round-trip times with a min/avg/max summary.
- **Device identity:** a self-signed Ed25519 certificate is made on first start
  and kept in `%ProgramData%\Illumination`. `illum fingerprint` shows it.
- **Encryption:** TLS 1.3 only, with certificates on **both** sides. Every
  handshake signature is checked.
- **Trust by pairing.** On a first connection the controller shows the host's
  fingerprint and asks whether to trust it. The host shows the device's
  fingerprint and asks whether to pair. After that, connections need no
  prompts. A host whose key changed is refused, and the error names the fix
  (`illum forget <host>`).
- `illum list` shows pinned hosts and paired devices. The trust store is
  versioned JSON.
- **Protocol:**
  - `hello` exchange with protocol version and a capability list
  - channel framing (`[channel][length][payload]`)
  - a send queue where input and cursor go before control, then video, then
    audio, then bulk, and where video can replace stale frames
- **Deadlines on every step:** name lookup, TCP connect, TLS, hello, prompts
  (unanswered counts as "no"), and idle sessions (closed after 30 s).

### Changed

- Clearer errors: an unreachable host says so in plain words instead of
  repeating the operating system's message. Logs have no colour codes when
  written to a file or pipe.

### Verified

- `cargo test`: 33 tests pass (unit tests, plus 9 loopback integration tests
  for pairing, refusals, a changed key and deadlines). `cargo clippy` is clean.
- On this PC over its own Tailscale address: the first connection asked once
  on each side, the reconnect needed no prompt, and the LAN address refused.
- Not yet run from a second machine (the laptop). See the M1 check in
  `docs/roadmap/roadmap.md`.

## [0.1.0] - 2026-10-19

First milestone, M0: the repository, the design and the plan. Nothing connects
yet.

### Added

- Rust workspace (edition 2024) under GPL-3.0-only, with two crates:
  `illum` (the single binary) and `illum-core` (shared code).
- `illum` picks its role from the command line, the same way AnyDesk's single
  binary does: the app window (no flag), `--service`, `--backend` and
  `--control`. Each role reports that it isn't implemented yet. Unknown flags
  exit with a usage message.
- Design docs:
  - `docs/architecture/`: roles and processes, one TLS 1.3 connection with
    prioritised channels, pairing by fingerprint, the video pipeline, input
    correctness rules, secure-desktop handling, permissions and session policy,
    with D2 diagrams.
  - `docs/lessons/`: evidence from AnyDesk (idle and a live direct session over
    Tailscale), RustDesk and Sunshine behind every decision.
  - `docs/roadmap/`: milestones M0–M7, each with a pass/fail check.
- Verified with `cargo build` and `cargo test` (3 tests passing).
