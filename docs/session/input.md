# Input and the secure desktop

How the controller's keyboard and mouse reach the host, and how a session carries on through
the lock screen and Windows' permission prompts.

Two words used throughout: the **host** is the machine being controlled, the **viewer** (or
controller) is the machine you sit at. The session window is the viewer's window that shows
the host's screen.

Related documents: [video.md](video.md) for what the pointer looks like,
[network-and-trust.md](../network-and-trust/network-and-trust.md) for channels, capabilities and
permissions.

## 6. Input

### Keyboard modes

A key press can be sent in two forms:

- as a **key position**: which physical key it is, whatever is printed on it. The host
  turns it into a character with its own keyboard layout and its own IME. (An IME, input
  method editor, is the part of the system that composes text from several key presses, as
  for Chinese or Japanese.)
- as **text**: the character or characters the press produced on the viewer, with the
  viewer's layout and IME. The host types them as Unicode text.

The keyboard mode chooses between them.

| Mode | What is sent | Which layout and IME apply | What is grabbed |
|---|---|---|---|
| `remote` (default) | Key positions | The host's | The whole keyboard |
| `local` | Characters typed here, including the local IME's result. Shortcuts and non-printing keys still go as key positions | The viewer's | System keys only |
| `auto` | As `remote` when both machines report the same layout, otherwise as `local` | The host's, or the viewer's | System keys only |

Details:

- `local` is for a host that lacks your language's IME. A press that produces a printable
  character with no Ctrl, Alt or Win held goes as text. Anything else (Ctrl+C, Enter, the
  arrows) goes as a key position.
- `auto` compares the host's layout with the viewer's. The host reports the layout of its
  front window at the start and whenever it changes (it looks once a second;
  `ControlMsg::KeyboardLayout`). While either layout is unknown, `auto` behaves as `remote`.
- `auto` never grabs the whole keyboard, even when it sends key positions. Choose `remote`
  for the full grab.
- Text arrives on the host through `SendInput` with `KEYEVENTF_UNICODE`, so it does not
  depend on the host's layout.

The mode is chosen with `illum view <host> --keyboard remote|local|auto` or in the session
window under **More → Keyboard**. A change in the window takes effect at once, and every key
held on the host is released first. The choice is kept per host, by the host's key
fingerprint, in `desk-choices.json` in the user's own folder. The order is: the command line,
else what was last chosen for this host, else `remote`. The pointer mode (`--pointer
remote|local`, **More → Pointer**) is kept the same way; what it does is in
[video.md](video.md).

`--view-only` sends no keyboard or mouse input at all. A session also opens view-only when
the host does not list the `input` capability.

### Keyboard grab

Windows keeps some keys for itself: the Windows key, Alt+Tab and others never reach an
ordinary window. To send them to the host the viewer installs a **low-level keyboard hook**
(`WH_KEYBOARD_LL`): a function Windows calls for every key event on the machine before any
program sees it, and which may swallow the event. The hook is `illum_win::SystemKeyHook`.

It works in one of two ways:

- **System keys only** (`local`, `auto`): it takes Win, the Menu key, Alt+Tab, Alt+Esc,
  Ctrl+Esc and Alt+F4. Everything else reaches the session window as usual.
- **The whole keyboard** (`remote`): it takes every key. The viewer's own Windows and its
  IME never see a press, so no local shortcut fires and no local IME composes.

Rules of the hook:

| Event | Sent to the host | Reaches the viewer's system |
|---|---|---|
| A grabbed key goes down | Yes | No |
| A grabbed key goes up | Yes | Yes, so a key that was down when the grab began does not stay stuck |
| CapsLock, NumLock (down or up) | Yes | Yes, so the lock lights stay right |
| Input this program injected itself (marked `OWN_INPUT` in `dwExtraInfo`) | No | Yes |
| Input another program injected (a remapper, an on-screen keyboard) | Counts as typing: the rows above apply | Counts as typing: the rows above apply |
| Ctrl+Alt+Shift+F2, F10, F11 | No. They are the viewer's own hotkeys | No |
| Ctrl+Alt+Del, Win+L | No. Windows takes them before any hook | Yes |

- **The grab is on while one of this program's windows is in front, and only then.** For
  every key the hook asks Windows which window is in front (`GetForegroundWindow`) and
  compares its process with its own. It does not rely on the window's focus events. To
  release the keyboard, bring another window to the front; the mouse is never grabbed.
- **The hook has a thread of its own** (`key-hook`) that does nothing but pump messages.
  Windows calls a low-level hook on the thread that installed it and silently removes a hook
  whose thread answers late. The window's thread draws and sets up the GPU, so the hook must
  not live there.
- The same thread looks ten times a second whether the program is in front and reports a
  change (`UiEvent::Front`). The window does its leaving work from that report: it tells the
  host to release everything and logs how many presses went to the host and how many stayed
  here. Counts only, never the keys.
- Under a full grab the local IME is left switched on. No press reaches it, so it has
  nothing to compose. Without a grab, in `remote` or `auto`-as-remote, the window turns its
  IME off; in `local` it turns it on.
- The hook and the window may both report the same key, and the hook reports the viewer's
  auto-repeat as more presses. `InputOut::key` drops a press of a key already held and a
  release of one that is not, so the host sees each once.
- If the hook cannot be installed, the session runs without it and says so. System keys
  then stay on the viewer.

**Constraint: the viewer must never enable winit raw device events.** Windows does not call
a process's own low-level keyboard hook while that process is registered for raw keyboard
input and holds the focus. winit registers for raw input whenever its window has focus,
unless told not to. With raw input on, the hook is installed, alive, passes every test on its
own, and is never called in the viewer: keys act locally and reach the host only through the
window. The viewer's event loop therefore sets `DeviceEvents::Never`
(`ViewerWindow::new` in `crates/illum-ui/src/lib.rs`). Do not turn device events on in the
viewer, raw mouse motion included, without moving the hook to another process.

The grab exists on Windows only. The wire format is the same on every system.

### Keys that cannot be grabbed

Windows takes Ctrl+Alt+Del and Win+L from the real keyboard before any hook runs. They act
on the viewer and never reach the host. The session window offers both under **More → Host**:

| Entry | Shown when | What happens |
|---|---|---|
| **Send Ctrl+Alt+Del** | The host lists the `sas` capability and the session has input | `ControlMsg::SecureAttention`; see §7 |
| **Lock** | The host lists the `lock` capability and the session has input | `ControlMsg::LockHost`; the host calls `LockWorkStation` |

The host carries out either only for a controller that has input there. `lock` is offered by
any Windows host that accepts input. `sas` needs the installed service (§7).

### Viewer hotkeys

Hotkeys of the viewer use the pattern `Ctrl+Alt+Shift+<key>` and are never sent to the host.
There are three:

| Hotkey | Does |
|---|---|
| Ctrl+Alt+Shift+F2 | Switches the picture between fit and original size |
| Ctrl+Alt+Shift+F10 | Switches Focus (the picture alone, in the window) on and off |
| Ctrl+Alt+Shift+F11 | Switches full screen on and off |

F10 and F11 go back to the bars when they switch a mode off. F11 is the key people know for
full screen from browsers; AnyDesk uses Ctrl+Alt+Shift+Return. Under a full grab the window
sees no press, so the hook's side picks the chord out (`GrabHotkeys`) and hands it to the
window.

### Key codes on the wire

A key position travels as its **USB HID usage**: the number the USB standard gives each
physical key, sent as `page << 16 | id` (`illum_core::keymap`). It belongs to no operating
system. A **scancode**, by contrast, is the number a PC keyboard sends for a key, and it is
what Windows injection takes; macOS has no such codes.

- The viewer maps the window library's physical key (the W3C `code` names, the same on every
  OS) to a usage: `illum_ui::input::KEY_USAGES`. The hook maps Windows scancodes to usages.
- The host maps the usage to what its injector takes: `keymap::windows_scancode`.
- Keys without a usage (F25 and up, Fn) are not sent.
- Two Windows quirks live in the host's table. Pause cannot be injected as a scancode and
  goes as its virtual key. NumLock is reported as `E0 45` by window messages and injected as
  plain `45`.
- A test on Windows checks the viewer's table against winit's own, key by key.

### Correctness rules

These keep one press from becoming many and keep keys from sticking.

1. **Only the host generates key repeats.** The viewer drops its own auto-repeat. The one
   exception is text in `local` mode: the host cannot repeat text, so the viewer's repeats
   are passed on for character keys.
2. **Every input event has a sequence number.** The host ignores an old number and a second
   key-down for a key it already holds.
3. **Held keys and buttons are released** when the session ends, when the viewer's window
   leaves the front, when the keyboard mode changes, and after a silence on the input
   channel.
4. **Keys pressed while the local IME is composing are not sent.** Its finished text is sent
   as text.

Host timings (`crates/illum/src/input.rs`, tested with a fake clock):

| Rule | Value |
|---|---|
| Repeat the most recently pressed key after | 500 ms |
| Repeat interval | 40 ms |
| Pause repeating after input silence of | 300 ms |
| Release everything after input silence of | 1 s |

While any key or button is held, the viewer sends a heartbeat every 200 ms, so silence
really means trouble.

### Mouse

- Positions are absolute, in pixels of the display being shown. The host is per-monitor DPI
  aware and converts to the normalised virtual-desktop coordinates `SendInput` takes.
- **Moves take the UDP media path, newest wins.** The viewer sends at most one move per 4 ms,
  the latest position, with its own sequence numbers. The host drops a move overtaken by a
  newer one. Until the media path is up, moves go on the TCP input channel.
- **Keys, buttons and wheels stay on TCP**, in order.
- **Buttons and wheels carry the pointer's position.** The host moves there before it
  presses, so a click lands where the mouse was even when the move before it is late or
  lost. The viewer also flushes a pending move before a click.
- A press off the picture (the margins beside it) is not sent. A release there is still
  sent, at the last place the pointer was on the picture.
- Five buttons are carried (left, right, middle, back, forward) and both wheel axes, in
  1/120 notch units.

The pointer you see is always drawn by the viewer's own system, so it never waits for the
host. Its look is in [video.md](video.md).

### Wire format

`illum_core::input`, binary, little-endian: `u8 kind | u32 seq | payload`.

| Kind | Message | Payload |
|---|---|---|
| 1 | key | `u32 usage`, `u8 down` |
| 2 | text | `u16 len`, UTF-8 bytes |
| 3 | move | `i32 x`, `i32 y` |
| 4 | button | `u8 button`, `u8 down`, `i32 x`, `i32 y` |
| 5 | wheel | `i32 dx`, `i32 dy`, `i32 x`, `i32 y` |
| 6 | heartbeat | none |
| 7 | release all | none |

### Input round trip

The host acknowledges every input event once it has injected it (`ControlMsg::InputAck`).
Moves from the UDP path are acknowledged at most ten times a second. The session window's
status line shows **"input N ms"**: the median round trip of the last second, or the last one
known. It covers the way to the host and the injection, not the picture's way back. Beside
the network round trip it shows whether an input delay is the network or the host.

### Input that cannot arrive

Windows keeps injected input out of two places, and `SendInput` reports no useful error for
either:

- the lock screen, the sign-in screen and permission prompts (§7), for a host that was not
  started by the installed service;
- a window that runs as administrator, for a host that does not.

Twice a second the host asks `illum_win::input_obstacle` whether one of these is the case.
It checks which desktop has the input, and whether the program in front is elevated while
the host is not; it does not wait for a refusal, because Windows reports success. A change
is logged on the host and sent to the viewer as `ControlMsg::InputBlocked { reason }` (an
empty reason means it is over) when the viewer lists the `input-notice` capability. The
viewer logs it and shows it as a notice in the session window. The notice only names the
problem.

### Platforms

Injection (`illum_win::WinInjector`), the keyboard grab and the layout reports are built for
Windows. A host on another system does not list `input`, and sessions with it are view-only.

## 7. Secure desktop (UAC, lock screen)

Windows shows the lock screen, the sign-in screen, the Ctrl+Alt+Del screen and its
permission prompts (UAC) on a separate desktop, named `Winlogon`, often called the **secure
desktop**. A thread captures and injects on the one desktop it is attached to, and an
ordinary program may not open the secure one.

### What the installed service makes possible

The installed Windows service runs as the system account and starts the host in the session
that has the screen, with the system's rights. Such a host can:

| Ability | How |
|---|---|
| Show and control the lock screen, the sign-in screen and UAC prompts | Its threads follow the screen to the other desktop |
| Send Ctrl+Alt+Del | It asks the service, which calls `SendSAS` |
| Type into windows that run as administrator | It runs above them |

A host that is merely run as a program has none of these. It stays on the ordinary desktop
and tells the controller why input does not arrive (§6, "Input that cannot arrive").

### Following the desktop

`illum_win::desktop::follow_input_desktop`:

1. The capture thread and the input thread each ask Windows, at most four times a second,
   which desktop has the screen (`OpenInputDesktop`).
2. If it is not the one the thread is on, the thread moves there (`SetThreadDesktop`).
3. The capture then starts again on the new desktop. The injector asks before each
   `SendInput`.

The session stays connected throughout.

Windows refuses a capture while it changes desktops: the old desktop may no longer be read.
The capture thread treats a refused capture as a switch first
(`ScreenCapture::ride_out`). It looks at once where the screen went, goes there, captures
anew, and sends nothing meanwhile. Only a capture that keeps failing for 15 seconds is
reported to the controller as an error. This applies to both capture methods, Desktop
Duplication and GDI.

### Ctrl+Alt+Del

Windows takes Ctrl+Alt+Del from the real keyboard only; it cannot be injected. A service
running as the system may ask for the same screen with `SendSAS` (SAS: secure attention
sequence).

1. The controller chooses **More → Send Ctrl+Alt+Del**, which sends
   `ControlMsg::SecureAttention`.
2. The host is not the service. It raises a named event, `Global\Illumination-SAS`, which
   the service created and only the system and administrators can open
   (`illum_win::sas::request`).
3. The service waits on that event in its loop and calls `SendSAS`.

The machine's policy must allow services to do this: `SoftwareSASGeneration`, bit 1, under
`HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System`. Installing the service
sets that bit if it is not set. Other remote-desktop programs use the same policy, so the
value is only ever widened and is left as it is on uninstall.

A host lists the `sas` capability only when it runs as the system. Otherwise the menu entry
is not shown.

### Lock

**More → Lock** locks the host (`LockWorkStation`), as Win+L would at its keyboard. It does
not need the service. What can be done on the lock screen afterwards does.

### What is not handled

- A host that is run without the installed service cannot show or control the secure
  desktop. While the host is locked or a UAC prompt is up, the controller is told why its
  input does not arrive.
- Only the session that has the physical screen (the console session) is served. The service
  moves the host there when the screen changes hands, which ends the running sessions.
- Following the desktop is built for Windows only.
