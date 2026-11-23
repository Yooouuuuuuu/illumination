// SPDX-License-Identifier: GPL-3.0-only
//! The app's window: this desk, the desk to reach, the desks you know, settings, and the
//! question about a desk that wants in.
//!
//! This module only draws and reports what the person did. [`AppState`] is what the rest of the
//! program knows (written by `illum::app`), [`Command`] is what the person asked for. The look
//! comes from the icon (`assets/icon`): a soft blue ground, navy outlines, the strong yellow
//! for the main action and for what is active now. Three more colours each mean one thing
//! wherever they appear: green is open or allowed, blue is waiting for an answer, coral is
//! closed, refused or broken.

use egui::{
    Align, Color32, CornerRadius, FontFamily, FontId, Frame, Layout, Margin, RichText, Sense,
    Stroke, StrokeKind, Ui, Vec2, vec2,
};
pub use illum_core::store::Permission;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

pub mod color {
    use egui::Color32;
    pub const TILE: Color32 = Color32::from_rgb(0xDD, 0xE6, 0xF6);
    pub const NAVY: Color32 = Color32::from_rgb(0x1E, 0x2A, 0x4A);
    pub const YELLOW: Color32 = Color32::from_rgb(0xFF, 0xE0, 0x12);
    pub const SOFT: Color32 = Color32::from_rgb(0xFF, 0xF6, 0x8D);
    /// Paler than `SOFT`: the bars inside a session window.
    pub const CREAM: Color32 = Color32::from_rgb(0xFF, 0xFC, 0xE3);
    pub const WHITE: Color32 = Color32::WHITE;
    /// Open, allowed: ready for connections, online, permission "always".
    pub const GREEN: Color32 = Color32::from_rgb(0xB9, 0xEF, 0xB0);
    /// Words that say something went well: green dark enough to read on white (5:1).
    pub const GREEN_TEXT: Color32 = Color32::from_rgb(0x1E, 0x7A, 0x34);
    /// Waiting for an answer: connecting, permission "ask".
    pub const BLUE: Color32 = Color32::from_rgb(0xC9, 0xDB, 0xFF);
    /// Closed, refused, broken: cannot be reached, permission "never", ending something.
    pub const CORAL: Color32 = Color32::from_rgb(0xFF, 0xB9, 0xA8);
    /// Secondary text: navy lightened, still 8:1 on white.
    pub const MUTED: Color32 = Color32::from_rgb(0x44, 0x50, 0x6E);
    /// Quiet outlines inside a card.
    pub const LINE: Color32 = Color32::from_rgb(0xC5, 0xD0, 0xE6);
    /// The lower part of a desk card: a warm grey, on a favourite as on any other.
    pub const STRIP: Color32 = Color32::from_rgb(0xEF, 0xED, 0xE6);
}
use color::*;

const OUTLINE: f32 = 2.0;
/// How tall the two cards of the first row are inside, so they end level: what the taller
/// one (the address to reach) needs, and no more.
const FIRST_ROW: f32 = 105.0;
const SEMIBOLD: &str = "semibold";
const HEADING: &str = "heading";

fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}
fn heading(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(HEADING.into()))
}
fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}
fn outline() -> Stroke {
    Stroke::new(OUTLINE, NAVY)
}

/// What the person chose in Settings. Kept in `settings.json` in the home folder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    // Outgoing: when I connect to another desk.
    pub max_bitrate_mbps: f64,
    pub max_fps: u32,
    pub latency_limit_ms: u32,
    pub allow_scale: bool,
    pub reconnect: bool,
    pub sound: bool,
    pub clipboard: bool,
    /// How see-through the "Exit" tab of a session window with its bars hidden is while the
    /// mouse is not on it, in percent (0: solid; 100: not shown).
    pub exit_tab_transparency: u32,
    // Incoming: when another desk connects here.
    /// What a connected desk may do, besides seeing the screen.
    pub let_control: bool,
    pub let_sound: bool,
    pub let_clipboard: bool,
    /// The permission of a desk that has none of its own.
    pub new_desks: Permission,
    /// A connected desk is disconnected after this long without input from it; 0: never.
    pub idle_minutes: u32,
    // General.
    /// Start with the system, in the tray, so this desk can be reached after a restart.
    pub start_on_boot: bool,
    /// The log files at debug level, for finding a problem; else info.
    pub detailed_logs: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            max_bitrate_mbps: 20.0,
            max_fps: 30,
            latency_limit_ms: 50,
            allow_scale: false,
            reconnect: true,
            sound: true,
            clipboard: true,
            exit_tab_transparency: 70,
            let_control: true,
            let_sound: true,
            let_clipboard: true,
            new_desks: Permission::Ask,
            idle_minutes: 30,
            start_on_boot: true,
            detailed_logs: false,
        }
    }
}

impl Settings {
    /// What decides who may connect here and what a connected desk may do: locked while a
    /// desk is connected.
    pub fn incoming(&self) -> (Permission, bool, bool, bool, u32) {
        (
            self.new_desks,
            self.let_control,
            self.let_sound,
            self.let_clipboard,
            self.idle_minutes,
        )
    }

    /// These settings with the Incoming ones of `other`.
    pub fn with_incoming_of(&self, other: &Settings) -> Settings {
        Settings {
            new_desks: other.new_desks,
            let_control: other.let_control,
            let_sound: other.let_sound,
            let_clipboard: other.let_clipboard,
            idle_minutes: other.idle_minutes,
            ..self.clone()
        }
    }

    /// The outgoing settings put back to what a new install has; the rest is kept.
    pub fn with_outgoing_reset(&self) -> Settings {
        let fresh = Settings::default();
        Settings {
            let_control: self.let_control,
            let_sound: self.let_sound,
            let_clipboard: self.let_clipboard,
            new_desks: self.new_desks,
            idle_minutes: self.idle_minutes,
            start_on_boot: self.start_on_boot,
            ..fresh
        }
    }
}

/// A desk this one knows: connected to, connected from, or both. One entry per machine.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Desk {
    /// 64 hex digits; what the desk is known by.
    pub fingerprint: String,
    /// The name to show: the person's own for it, else the machine's.
    pub name: String,
    /// The person's own name, empty when they gave none (for the edit dialog).
    pub own_name: String,
    /// Its system as it said ("Windows 11"); empty if it did not.
    pub os: String,
    /// Where it was last seen, without a port.
    pub address: String,
    /// What to dial to connect to it; `None` for a desk that only ever came here.
    pub connect_to: Option<String>,
    /// Its own permission; `None`: as in Settings.
    pub permission: Option<Permission>,
    pub favorite: bool,
    /// Unix seconds; 0: never.
    pub last_out: u64,
    pub last_in: u64,
    pub tried_in: u64,
}

/// A desk connected to this one right now.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Guest {
    pub name: String,
    pub address: String,
    /// Unix seconds when it connected.
    pub since: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Incoming {
    #[default]
    Off,
    Listening,
    /// Could not listen; the reason, in words.
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Dial {
    #[default]
    Idle,
    /// Reaching this address now.
    Dialing(String),
    Failed {
        target: String,
        message: String,
    },
}

/// A desk that wants to connect here, waiting for the person's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub id: u64,
    pub name: String,
    pub address: String,
    /// 64 hex digits.
    pub fingerprint: String,
    /// The desk's system as it said ("Windows 11"); empty if it did not.
    pub os: String,
}

/// The person's answer to a question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    No,
    /// Yes, this time; nothing is remembered.
    Once,
    /// Yes, and from now on without asking.
    Always,
    /// No, and from now on without asking.
    Never,
}

/// Updates: what the last check found, and how installing goes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Updates {
    /// The newer release a check found.
    pub available: Option<NewRelease>,
    pub checked: Checked,
    pub progress: Progress,
    /// Whether this copy can update itself: an installed desk whose service runs.
    pub installed: bool,
    /// The installer runs: the window closes so the program can be replaced.
    pub closing: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewRelease {
    pub version: String,
    /// Its changelog entry.
    pub notes: String,
}

/// The last check for updates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Checked {
    #[default]
    Not,
    Checking,
    /// When (Unix seconds); `available` says what it found.
    Done(u64),
    /// A check the person asked for did not get an answer. One done by itself that fails is
    /// not said.
    Failed(String),
}

/// An update on its way.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Progress {
    #[default]
    Idle,
    /// Percent, when the size is known.
    Downloading(Option<u8>),
    Installing,
    Failed(String),
}

/// One line of a release's notes, as shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteLine {
    Heading(String),
    Bullet(String),
    Text(String),
}

/// A changelog entry as lines to show: headings, bullets (with their wrapped lines joined)
/// and plain text, without the Markdown marks.
pub fn note_lines(notes: &str) -> Vec<NoteLine> {
    let mut lines: Vec<NoteLine> = Vec::new();
    // Whether the next line may carry on the last one (no blank line came between).
    let mut open = false;
    for raw in notes.lines() {
        let line = raw.trim();
        if line.is_empty() {
            open = false;
            continue;
        }
        if line.starts_with('#') {
            lines.push(NoteLine::Heading(plain(
                line.trim_start_matches('#').trim(),
            )));
            open = false;
        } else if let Some(item) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            lines.push(NoteLine::Bullet(plain(item)));
            open = true;
        } else if open && let Some(NoteLine::Bullet(last) | NoteLine::Text(last)) = lines.last_mut()
        {
            last.push(' ');
            last.push_str(&plain(line));
        } else {
            lines.push(NoteLine::Text(plain(line)));
            open = true;
        }
    }
    lines
}

/// Markdown's marks taken out: code, and links (their words kept). Bold stays, for drawing.
fn plain(text: &str) -> String {
    let text = text.replace('`', "");
    let mut out = String::new();
    let mut rest = text.as_str();
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        match after.find("](").and_then(|mid| {
            after[mid + 2..]
                .find(')')
                .map(|close| (mid, mid + 2 + close))
        }) {
            Some((mid, close)) => {
                out.push_str(&rest[..open]);
                out.push_str(&after[..mid]);
                rest = &after[close + 1..];
            }
            None => {
                out.push_str(&rest[..=open]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Everything the window shows that it does not own.
#[derive(Debug, Clone, Default)]
pub struct AppState {
    pub version: String,
    pub device_name: String,
    /// The addresses other desks can dial, best first. Empty: no tailnet address was found.
    pub addresses: Vec<String>,
    /// 64 hex digits.
    pub fingerprint: String,
    pub incoming: Incoming,
    /// Names of the desks connected to this one right now.
    pub sessions: Vec<String>,
    /// The same desks with where they are and since when, for the "connected here" list.
    pub guests: Vec<Guest>,
    /// How many sessions this desk has open to other desks.
    pub outgoing: usize,
    pub desks: Vec<Desk>,
    /// Whether each desk that can be dialled answers right now, by `connect_to`. Missing:
    /// not looked yet.
    pub online: HashMap<String, bool>,
    pub dial: Dial,
    pub questions: VecDeque<Question>,
    pub settings: Settings,
    /// Now, in Unix seconds, for "today" and "yesterday" on the cards.
    pub now: u64,
    pub update: Updates,
}

/// What the person asked for.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Connect {
        target: String,
        view_only: bool,
        mute: bool,
    },
    /// Clear a failed attempt's message.
    DismissDial,
    Answer {
        id: u64,
        verdict: Verdict,
    },
    /// What the edit dialog was left with for one desk.
    EditDesk {
        fingerprint: String,
        /// The person's own name for it; empty gives it its machine name back.
        name: String,
        permission: Option<Permission>,
        favorite: bool,
    },
    Favorite {
        fingerprint: String,
        on: bool,
    },
    /// Forget everything about a desk.
    ForgetDesk(String),
    /// End the sessions of the desk of this name that is connected to this one.
    EndSession(String),
    SaveSettings(Settings),
    /// Look for a newer release now.
    CheckUpdates,
    /// Install the newer release.
    UpdateNow,
    /// Stop the download of an update.
    CancelUpdate,
    /// Bring the session windows open to other desks to the front.
    RaiseSessions,
    /// Send the window to the tray; the app goes on.
    Hide,
    /// Close the app: every connection ends and this desk cannot be reached until it is
    /// opened again.
    Quit,
}

/// A key as four rows of four groups: short enough lines to compare across two screens.
pub fn key_rows(fingerprint: &str) -> Vec<String> {
    let groups: Vec<String> = fingerprint
        .to_ascii_uppercase()
        .as_bytes()
        .chunks(4)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect();
    groups.chunks(4).map(|row| row.join(" ")).collect()
}

/// The start of a key, for telling desks apart in a list (not for checking one).
pub fn key_start(fingerprint: &str) -> String {
    key_rows(fingerprint).into_iter().next().unwrap_or_default()
}

/// An address as shown: without the port when it is the usual one.
pub fn shown_address(address: &str) -> &str {
    address
        .strip_suffix(&format!(":{}", illum_core::protocol::DEFAULT_PORT))
        .unwrap_or(address)
}

/// A day as `2026-11-13`, from Unix seconds (UTC; civil-from-days, Howard Hinnant).
pub fn date(unix: u64) -> String {
    let z = (unix / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// The fonts: Inter for text, Space Grotesk for headings, JetBrains Mono for addresses and keys
/// (`assets/fonts`, all under the Open Font License). They hold Latin only, so a system font
/// with Chinese, Japanese and Korean is added behind them when the machine has one: desk names
/// are whatever their owners called them.
pub fn fonts() -> egui::FontDefinitions {
    use egui::FontData;
    let mut fonts = egui::FontDefinitions::default();
    let mut add = |name: &str, data: FontData| {
        fonts.font_data.insert(name.to_string(), Arc::new(data));
    };
    add(
        "inter",
        FontData::from_static(include_bytes!("../../../assets/fonts/Inter-Regular.ttf")),
    );
    add(
        "inter-semibold",
        FontData::from_static(include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf")),
    );
    add(
        "space-grotesk",
        FontData::from_static(include_bytes!(
            "../../../assets/fonts/SpaceGrotesk-Bold.ttf"
        )),
    );
    add(
        "jetbrains-mono",
        FontData::from_static(include_bytes!(
            "../../../assets/fonts/JetBrainsMono-Medium.ttf"
        )),
    );
    let wide = system_wide_font();
    if let Some(data) = wide.clone() {
        add("system-wide", data);
    }
    // What egui falls back to today (its own Latin and symbol fonts), kept behind ours.
    let fallback = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let family = |first: &str| {
        let mut list = vec![first.to_string()];
        if wide.is_some() {
            list.push("system-wide".to_string());
        }
        list.extend(fallback.iter().cloned());
        list
    };
    fonts
        .families
        .insert(FontFamily::Proportional, family("inter"));
    fonts
        .families
        .insert(FontFamily::Name(SEMIBOLD.into()), family("inter-semibold"));
    fonts
        .families
        .insert(FontFamily::Name(HEADING.into()), family("space-grotesk"));
    fonts
        .families
        .insert(FontFamily::Monospace, family("jetbrains-mono"));
    fonts
}

/// A font this machine has for Chinese, Japanese and Korean text, if any of the usual ones.
fn system_wide_font() -> Option<egui::FontData> {
    const CANDIDATES: &[&str] = &[
        "C:\\Windows\\Fonts\\msjh.ttc",
        "C:\\Windows\\Fonts\\msyh.ttc",
        "C:\\Windows\\Fonts\\YuGothR.ttc",
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
    ];
    CANDIDATES
        .iter()
        .find_map(|path| std::fs::read(path).ok())
        .map(egui::FontData::from_owned)
}

/// Fonts and the look of the stock widgets (checkboxes, text fields, menus).
pub fn apply_style(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.set_theme(egui::Theme::Light);
    ctx.all_styles_mut(|style| {
        use egui::TextStyle;
        style.text_styles = [
            (
                TextStyle::Small,
                FontId::new(13.0, FontFamily::Proportional),
            ),
            (TextStyle::Body, FontId::new(15.0, FontFamily::Proportional)),
            (TextStyle::Button, semibold(14.0)),
            (TextStyle::Heading, heading(22.0)),
            (TextStyle::Monospace, mono(14.0)),
        ]
        .into();
        style.spacing.item_spacing = vec2(10.0, 10.0);
        style.spacing.button_padding = vec2(14.0, 9.0);
        style.spacing.interact_size.y = 36.0;
        style.spacing.icon_width = 20.0;
        style.spacing.icon_spacing = 10.0;
        let v = &mut style.visuals;
        v.override_text_color = Some(NAVY);
        v.panel_fill = TILE;
        v.window_fill = WHITE;
        v.extreme_bg_color = WHITE;
        v.selection.bg_fill = YELLOW;
        v.selection.stroke = Stroke::new(1.0, NAVY);
        v.hyperlink_color = NAVY;
        let radius = CornerRadius::same(10);
        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.corner_radius = radius;
            w.bg_stroke = outline();
            w.fg_stroke = Stroke::new(2.0, NAVY);
            w.bg_fill = WHITE;
            w.weak_bg_fill = WHITE;
            w.expansion = 0.0;
        }
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, NAVY);
        v.widgets.hovered.bg_fill = SOFT;
        v.widgets.hovered.weak_bg_fill = SOFT;
        v.widgets.active.bg_fill = YELLOW;
        v.widgets.active.weak_bg_fill = YELLOW;
    });
}

fn card(ui: &mut Ui, fill: Color32, add: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(fill)
        .stroke(outline())
        .corner_radius(CornerRadius::same(16))
        .inner_margin(Margin::same(24))
        .show(ui, |ui| {
            // Whatever spacing the row of cards used, inside a card it is the usual one.
            ui.spacing_mut().item_spacing.x = 10.0;
            add(ui)
        });
}

fn caps(text: &str) -> RichText {
    RichText::new(text.to_uppercase())
        .font(heading(13.0))
        .extra_letter_spacing(1.0)
}

fn muted(text: impl Into<String>) -> RichText {
    RichText::new(text).size(14.0).color(MUTED)
}

fn button(text: &str, fill: Color32, height: f32) -> egui::Button<'static> {
    egui::Button::new(
        RichText::new(text.to_string())
            .font(semibold(14.0))
            .color(NAVY),
    )
    .fill(fill)
    .stroke(outline())
    .corner_radius(CornerRadius::same(12))
    .min_size(vec2(0.0, height))
}

/// The main action of a screen.
fn primary(text: &str) -> egui::Button<'static> {
    egui::Button::new(
        RichText::new(text.to_string())
            .font(heading(16.0))
            .color(NAVY),
    )
    .fill(YELLOW)
    .stroke(outline())
    .corner_radius(CornerRadius::same(12))
    .min_size(vec2(0.0, 48.0))
}

fn text_field<'a>(text: &'a mut String, hint: &str, font: FontId) -> egui::TextEdit<'a> {
    egui::TextEdit::singleline(text)
        .hint_text(RichText::new(hint.to_string()).color(MUTED))
        .font(font)
        .margin(Margin::symmetric(14, 12))
        .text_color(NAVY)
}

/// A key in a box, for comparing with the other screen.
fn key_box(ui: &mut Ui, fingerprint: &str, fill: Color32, size: f32) {
    Frame::new()
        .fill(fill)
        .stroke(outline())
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.vertical_centered(|ui| {
                for row in key_rows(fingerprint) {
                    ui.label(RichText::new(row).font(mono(size)));
                }
            });
        });
}

/// The icon's two screens, drawn (crisp at any size, no texture).
fn logo(ui: &mut Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let p = ui.painter();
    let s = size / 1024.0;
    let at = |x: f32, y: f32, w: f32, h: f32| {
        egui::Rect::from_min_size(rect.min + vec2(x * s, y * s), vec2(w * s, h * s))
    };
    let line = Stroke::new((size / 18.0).max(1.5), NAVY);
    p.rect(
        at(14.0, 14.0, 996.0, 996.0),
        CornerRadius::same((216.0 * s) as u8),
        TILE,
        line,
        StrokeKind::Inside,
    );
    let r = CornerRadius::same((24.0 * s).max(1.0) as u8);
    p.rect(
        at(130.0, 303.0, 478.0, 348.0),
        r,
        YELLOW,
        line,
        StrokeKind::Middle,
    );
    p.rect(
        at(417.0, 373.0, 478.0, 348.0),
        r,
        SOFT,
        line,
        StrokeKind::Middle,
    );
}

/// A selectable choice in a row of them: filled with its colour when it is the chosen one.
fn choice(ui: &mut Ui, text: &str, chosen: bool, fill: Color32, size: Vec2) -> bool {
    let button = egui::Button::new(
        RichText::new(text.to_string())
            .font(semibold(14.0))
            .color(NAVY),
    )
    .fill(if chosen { fill } else { WHITE })
    .stroke(Stroke::new(OUTLINE, if chosen { NAVY } else { LINE }))
    .corner_radius(CornerRadius::same(12))
    .min_size(size);
    ui.add(button).clicked() && !chosen
}

/// The words and the colour of a permission.
pub fn permission_look(permission: Permission) -> (&'static str, Color32) {
    match permission {
        Permission::Always => ("Always allow", GREEN),
        Permission::Ask => ("Ask", BLUE),
        Permission::Never => ("Never allow", CORAL),
    }
}

/// A day as the cards say it: today, yesterday, else the date.
pub fn day(now: u64, then: u64) -> String {
    match (now / 86_400).checked_sub(then / 86_400) {
        Some(0) => "today".to_string(),
        Some(1) => "yesterday".to_string(),
        _ => format!("on {}", date(then)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Page {
    #[default]
    Home,
    Settings,
}

/// Which desks the list shows. A desk can be under several.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Favorites,
    Recent,
    CameIn,
}

/// One setting: an entry in the menu, a place to jump to, and what the unsaved marks count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    StartOnBoot,
    Key,
    Logs,
    Limits,
    Sound,
    Clipboard,
    Reconnect,
    Shrink,
    Tab,
    Permission,
    Actions,
    Idle,
    /// The desks that are always allowed: a list, not a setting that waits for Save.
    Trusted,
    About,
}

/// The sections of Settings, each with its settings in the order they are drawn.
const MENU: [(&str, &[Item]); 5] = [
    ("General", &[Item::StartOnBoot, Item::Key, Item::Logs]),
    (
        "Connection",
        &[
            Item::Limits,
            Item::Sound,
            Item::Clipboard,
            Item::Reconnect,
            Item::Shrink,
            Item::Tab,
        ],
    ),
    ("Incoming", &[Item::Permission, Item::Actions, Item::Idle]),
    ("Trusted desks", &[Item::Trusted]),
    ("About", &[Item::About]),
];

/// The settings that differ between what is saved and what is on screen.
fn unsaved(saved: &Settings, shown: &Settings) -> Vec<Item> {
    let mut items = Vec::new();
    let mut differs = |item, differs: bool| {
        if differs {
            items.push(item);
        }
    };
    differs(
        Item::StartOnBoot,
        saved.start_on_boot != shown.start_on_boot,
    );
    differs(Item::Logs, saved.detailed_logs != shown.detailed_logs);
    differs(Item::Permission, saved.new_desks != shown.new_desks);
    differs(
        Item::Actions,
        (saved.let_control, saved.let_sound, saved.let_clipboard)
            != (shown.let_control, shown.let_sound, shown.let_clipboard),
    );
    differs(Item::Idle, saved.idle_minutes != shown.idle_minutes);
    differs(
        Item::Limits,
        (
            saved.max_bitrate_mbps,
            saved.max_fps,
            saved.latency_limit_ms,
        ) != (
            shown.max_bitrate_mbps,
            shown.max_fps,
            shown.latency_limit_ms,
        ),
    );
    differs(Item::Sound, saved.sound != shown.sound);
    differs(Item::Clipboard, saved.clipboard != shown.clipboard);
    differs(Item::Reconnect, saved.reconnect != shown.reconnect);
    differs(Item::Shrink, saved.allow_scale != shown.allow_scale);
    differs(
        Item::Tab,
        saved.exit_tab_transparency != shown.exit_tab_transparency,
    );
    items
}

/// A desk as the edit dialog holds it until Save.
struct Editing {
    desk: Desk,
    name: String,
    permission: Option<Permission>,
}

/// What the window itself remembers between frames.
#[derive(Default)]
pub struct HomeUi {
    page: Page,
    address: String,
    view_only: bool,
    mute: bool,
    filter: String,
    /// The tab the person chose; until then, Favorites if there are any, else Recent.
    tab: Option<Tab>,
    editing: Option<Editing>,
    /// The settings as edited and not yet saved, while the Settings page is open.
    pending: Option<Settings>,
    /// What Save last sent: "Saved" shows while the page still holds it, saved.
    saved: Option<Settings>,
    /// The setting the menu was asked to bring into view.
    jump: Option<Item>,
    /// Asked to leave Settings with unsaved changes.
    leaving: bool,
    /// The "connected here" list is open.
    connected: bool,
    /// Asked to close while something is connected.
    closing: bool,
    /// The update dialog is open.
    update: bool,
}

impl HomeUi {
    /// Opens on Settings (for pictures of that page).
    pub fn on_settings() -> HomeUi {
        HomeUi {
            page: Page::Settings,
            ..HomeUi::default()
        }
    }

    /// Opens on the "Came in" tab (for pictures).
    pub fn on_came_in() -> HomeUi {
        HomeUi {
            tab: Some(Tab::CameIn),
            ..HomeUi::default()
        }
    }

    /// Shows the list of desks connected here (the tray's "Connected here").
    pub fn show_connected(&mut self) {
        self.connected = true;
    }

    /// The person asked to close. Returns whether that can go ahead at once; with something
    /// connected it cannot, and the question about it is shown instead.
    pub fn ask_close(&mut self, state: &AppState) -> bool {
        let connections = state.sessions.len() + state.outgoing;
        self.closing = connections > 0;
        connections == 0
    }

    /// Opens on the close question (for pictures).
    pub fn closing() -> HomeUi {
        HomeUi {
            closing: true,
            ..HomeUi::default()
        }
    }

    /// Opens on the "connected here" list (for pictures).
    pub fn connected() -> HomeUi {
        HomeUi {
            connected: true,
            ..HomeUi::default()
        }
    }

    /// Opens on the update dialog (for pictures).
    pub fn updating() -> HomeUi {
        HomeUi {
            update: true,
            ..HomeUi::default()
        }
    }

    /// Opens with the edit dialog for one desk (for pictures).
    pub fn editing(desk: &Desk) -> HomeUi {
        let mut ui = HomeUi::default();
        ui.edit(desk);
        ui
    }

    fn edit(&mut self, desk: &Desk) {
        self.editing = Some(Editing {
            desk: desk.clone(),
            name: desk.name.clone(),
            permission: desk.permission,
        });
    }

    /// Draws one frame and returns what the person asked for.
    pub fn draw(&mut self, ui: &mut Ui, state: &AppState) -> Vec<Command> {
        let mut out = Vec::new();
        if self.page == Page::Settings && self.pending.is_none() {
            self.pending = Some(state.settings.clone());
        }
        self.top_bar(ui, state, &mut out);
        egui::CentralPanel::default()
            .frame(Frame::new().fill(TILE))
            .show(ui, |ui| match self.page {
                Page::Home => {
                    egui::ScrollArea::vertical()
                        .auto_shrink(false)
                        .show(ui, |ui| {
                            let full = ui.available_width();
                            let width = (full - 64.0).clamp(280.0, 1056.0);
                            let side = ((full - width) / 2.0).max(0.0);
                            ui.add_space(28.0);
                            ui.horizontal_top(|ui| {
                                ui.add_space(side);
                                ui.vertical(|ui| {
                                    ui.set_width(width);
                                    ui.spacing_mut().item_spacing.y = 12.0;
                                    self.home(ui, state, width, &mut out);
                                });
                            });
                            ui.add_space(32.0);
                        });
                }
                Page::Settings => self.settings(ui, state, &mut out),
            });
        self.edit_dialog(ui.ctx(), state, &mut out);
        self.leave_dialog(ui.ctx(), &mut out);
        self.connected_dialog(ui.ctx(), state, &mut out);
        self.close_dialog(ui.ctx(), state, &mut out);
        self.update_dialog(ui.ctx(), state, &mut out);
        if let Some(q) = state.questions.front() {
            question(ui.ctx(), q, &mut out);
        }
        out
    }

    fn top_bar(&mut self, ui: &mut Ui, state: &AppState, out: &mut Vec<Command>) {
        let bar = egui::Panel::top("bar")
            .frame(
                Frame::new()
                    .fill(WHITE)
                    .inner_margin(Margin::symmetric(24, 12)),
            )
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| match self.page {
                    Page::Home => {
                        logo(ui, 36.0);
                        if ui.available_width() > 430.0 {
                            let name = ui.label(RichText::new("Illumination").font(heading(20.0)));
                            // The version, small and grey beside the name, on its baseline.
                            let version = ui.painter().layout_no_wrap(
                                format!("v{}", state.version),
                                mono(10.0),
                                MUTED,
                            );
                            let (spot, _) = ui.allocate_exact_size(
                                vec2(version.size().x - 4.0, name.rect.height()),
                                Sense::hover(),
                            );
                            let at = egui::pos2(
                                spot.left() - 4.0,
                                name.rect.bottom() - version.size().y - 4.0,
                            );
                            ui.painter().galley(at, version, MUTED);
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.add(button("Settings", WHITE, 40.0)).clicked() {
                                self.page = Page::Settings;
                            }
                            if status_pill(ui, state) {
                                self.connected = true;
                            }
                            if outgoing_pill(ui, state) {
                                out.push(Command::RaiseSessions);
                            }
                            if update_pill(ui, state) {
                                self.update = true;
                            }
                        });
                    }
                    Page::Settings => {
                        let changes = self
                            .pending
                            .as_ref()
                            .map_or(0, |p| unsaved(&state.settings, p).len());
                        if ui.add(button("Back", WHITE, 40.0)).clicked() {
                            if changes == 0 {
                                self.leave();
                            } else {
                                self.leaving = true;
                            }
                        }
                        ui.label(RichText::new("Settings").font(heading(20.0)));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            // Always in sight, wherever the page is scrolled to. Right to
                            // left: Discard at the edge, Save before it.
                            if ui
                                .add_enabled(changes > 0, button("Discard", WHITE, 40.0))
                                .clicked()
                            {
                                self.pending = Some(state.settings.clone());
                            }
                            let save = ui.add_enabled(
                                changes > 0,
                                primary("Save").min_size(vec2(96.0, 40.0)),
                            );
                            if save.clicked()
                                && let Some(pending) = &self.pending
                            {
                                self.saved = Some(pending.clone());
                                out.push(Command::SaveSettings(pending.clone()));
                            }
                            if changes == 0 && self.saved.is_some() && self.saved == self.pending {
                                ui.label(
                                    RichText::new("Saved")
                                        .font(semibold(14.0))
                                        .color(GREEN_TEXT),
                                );
                            } else if changes > 0 {
                                let text = if changes == 1 {
                                    "1 unsaved change".to_string()
                                } else {
                                    format!("{changes} unsaved changes")
                                };
                                ui.label(RichText::new(text).font(semibold(14.0)));
                            }
                        });
                    }
                });
            });
        let rect = bar.response.rect;
        // Inside the bar, so the page below does not paint over half of it.
        let y = rect.bottom() - OUTLINE / 2.0;
        ui.painter().line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            Stroke::new(OUTLINE, NAVY),
        );
    }

    fn leave(&mut self) {
        self.page = Page::Home;
        self.pending = None;
        self.saved = None;
        self.leaving = false;
        self.jump = None;
    }

    fn home(&mut self, ui: &mut Ui, state: &AppState, width: f32, out: &mut Vec<Command>) {
        // Side by side when there is room, one under the other on a narrow window.
        let gap = 24.0;
        let mut to_incoming = false;
        if width >= 760.0 {
            let each = (width - gap) / 2.0;
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                ui.allocate_ui_with_layout(vec2(each, 0.0), Layout::top_down(Align::Min), |ui| {
                    to_incoming = this_desk(ui, state, each, FIRST_ROW);
                });
                ui.allocate_ui_with_layout(vec2(each, 0.0), Layout::top_down(Align::Min), |ui| {
                    self.remote_desk(ui, state, each, FIRST_ROW, out)
                });
            });
        } else {
            to_incoming = this_desk(ui, state, width, 0.0);
            ui.add_space(12.0);
            self.remote_desk(ui, state, width, 0.0, out);
        }
        if to_incoming {
            self.page = Page::Settings;
            self.jump = Some(Item::Permission);
        }
        ui.add_space(20.0);
        self.desks(ui, state, width, out);
    }

    fn remote_desk(
        &mut self,
        ui: &mut Ui,
        state: &AppState,
        width: f32,
        min_height: f32,
        out: &mut Vec<Command>,
    ) {
        card(ui, WHITE, |ui| {
            ui.set_width(width - 52.0);
            ui.set_min_height(min_height);
            ui.horizontal(|ui| {
                ui.label(caps("Remote desk"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.checkbox(&mut self.mute, "Mute its sound");
                    ui.add_space(8.0);
                    ui.checkbox(&mut self.view_only, "View only");
                });
            });
            let dialing = matches!(state.dial, Dial::Dialing(_));
            let mut go = false;
            ui.horizontal(|ui| {
                let connect_width = 140.0;
                let field = ui.add_sized(
                    [ui.available_width() - connect_width - 10.0, 48.0],
                    text_field(&mut self.address, "100.100.101.2", mono(17.0)),
                );
                go |= field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                // Blue while the other side is being asked.
                let connect = if dialing {
                    primary("Connecting…").fill(BLUE)
                } else {
                    primary("Connect")
                };
                go |= ui
                    .add_enabled(!dialing, connect.min_size(vec2(connect_width, 48.0)))
                    .clicked();
            });
            if go && !dialing && !self.address.trim().is_empty() {
                out.push(Command::Connect {
                    target: self.address.trim().to_string(),
                    view_only: self.view_only,
                    mute: self.mute,
                });
            }
            if let Dial::Failed { message, .. } = &state.dial {
                Frame::new()
                    .fill(CORAL)
                    .stroke(outline())
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.add(button("OK", WHITE, 32.0)).clicked() {
                                    out.push(Command::DismissDial);
                                }
                                ui.with_layout(
                                    Layout::left_to_right(Align::Center).with_main_wrap(true),
                                    |ui| ui.label(RichText::new(message).size(14.0)),
                                );
                            });
                        });
                    });
            }
        });
    }

    fn desks(&mut self, ui: &mut Ui, state: &AppState, width: f32, out: &mut Vec<Command>) {
        let tab = self.tab.unwrap_or_else(|| {
            if state.desks.iter().any(|d| d.favorite) {
                Tab::Favorites
            } else {
                Tab::Recent
            }
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Desks").font(heading(22.0)));
            ui.add_space(6.0);
            Frame::new()
                .stroke(outline())
                .corner_radius(CornerRadius::same(12))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.horizontal(|ui| {
                        let tabs = [
                            (Tab::Favorites, "Favorites"),
                            (Tab::Recent, "Recent"),
                            (Tab::CameIn, "Came in"),
                        ];
                        for (i, (value, label)) in tabs.into_iter().enumerate() {
                            let on = value == tab;
                            let radius = CornerRadius {
                                nw: if i == 0 { 10 } else { 0 },
                                sw: if i == 0 { 10 } else { 0 },
                                ne: if i == 2 { 10 } else { 0 },
                                se: if i == 2 { 10 } else { 0 },
                            };
                            let pick = egui::Button::new(
                                RichText::new(label).font(semibold(14.0)).color(NAVY),
                            )
                            .fill(if on { YELLOW } else { WHITE })
                            .stroke(Stroke::NONE)
                            .corner_radius(radius)
                            .min_size(vec2(0.0, 36.0));
                            if ui.add(pick).clicked() {
                                self.tab = Some(value);
                            }
                        }
                    });
                });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_sized(
                    [220.0, 40.0],
                    text_field(&mut self.filter, "Find a desk", FontId::proportional(15.0)),
                );
            });
        });
        let needle = self.filter.trim().to_lowercase();
        let mut shown: Vec<&Desk> = state
            .desks
            .iter()
            .filter(|d| match tab {
                Tab::Favorites => d.favorite,
                Tab::Recent => d.last_out > 0,
                Tab::CameIn => d.last_in > 0 || d.tried_in > 0,
            })
            .filter(|d| {
                needle.is_empty()
                    || d.name.to_lowercase().contains(&needle)
                    || d.address.contains(&needle)
            })
            .collect();
        match tab {
            Tab::Favorites => shown.sort_by_key(|d| d.name.to_lowercase()),
            Tab::Recent => shown.sort_by_key(|d| std::cmp::Reverse(d.last_out)),
            Tab::CameIn => shown.sort_by_key(|d| std::cmp::Reverse(d.last_in.max(d.tried_in))),
        }
        if shown.is_empty() {
            ui.label(muted(match (tab, needle.is_empty()) {
                (_, false) => "No desk matches.",
                (Tab::Favorites, _) => "No favorites yet. Add one from a desk's menu.",
                (Tab::Recent, _) => "No desks yet. Desks you connect to appear here.",
                (Tab::CameIn, _) => "No desk has connected here yet.",
            }));
            return;
        }
        // As many columns as fit at 240 px or wider, all the same width.
        let gap = 20.0;
        let columns = (((width + gap) / (240.0 + gap)).floor() as usize).max(1);
        let each = (width - gap * (columns as f32 - 1.0)) / columns as f32;
        let dialing = matches!(state.dial, Dial::Dialing(_));
        for row in shown.chunks(columns) {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for desk in row {
                    ui.allocate_ui_with_layout(
                        vec2(each, 0.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            let did = desk_card(ui, desk, state, tab, each, dialing);
                            match did {
                                Some(OnCard::Connect) => {
                                    if let Some(target) = &desk.connect_to {
                                        out.push(Command::Connect {
                                            target: target.clone(),
                                            view_only: self.view_only,
                                            mute: self.mute,
                                        });
                                    }
                                }
                                Some(OnCard::Edit) => self.edit(desk),
                                Some(OnCard::Favorite) => out.push(Command::Favorite {
                                    fingerprint: desk.fingerprint.clone(),
                                    on: !desk.favorite,
                                }),
                                None => {}
                            }
                        },
                    );
                }
            });
            ui.add_space(8.0);
        }
    }

    fn edit_dialog(&mut self, ctx: &egui::Context, state: &AppState, out: &mut Vec<Command>) {
        let Some(editing) = &mut self.editing else {
            return;
        };
        let mut close = false;
        dialog(ctx, "edit-desk", |ui| {
            ui.set_width(392.0);
            ui.spacing_mut().item_spacing.y = 12.0;
            if !editing.desk.os.is_empty() {
                ui.label(caps(&editing.desk.os).color(MUTED));
            }
            ui.label(
                RichText::new(format!(
                    "{} · key {}",
                    editing.desk.address,
                    key_start(&editing.desk.fingerprint)
                ))
                .font(mono(13.0))
                .color(MUTED),
            );
            ui.label(RichText::new("Name").font(semibold(14.0)));
            ui.add_sized(
                [ui.available_width(), 48.0],
                text_field(&mut editing.name, "", heading(17.0)),
            );
            ui.label(RichText::new("When it connects here").font(semibold(14.0)));
            let locked = lock_note(state);
            if let Some(note) = &locked {
                ui.label(muted(note));
            }
            let each = vec2((ui.available_width() - 10.0) / 2.0, 44.0);
            // Without a rule of its own it follows Settings: said with what that is now.
            let (usual, _) = permission_look(state.settings.new_desks);
            let usual = format!("Default ({})", usual.to_lowercase());
            let mut choices: Vec<(Option<Permission>, &str, Color32)> =
                vec![(None, usual.as_str(), SOFT)];
            for permission in [Permission::Always, Permission::Ask, Permission::Never] {
                let (word, fill) = permission_look(permission);
                choices.push((Some(permission), word, fill));
            }
            for pair in choices.chunks(2) {
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(locked.is_none(), |ui| {
                        for (value, label, fill) in pair {
                            if choice(ui, label, editing.permission == *value, *fill, each) {
                                editing.permission = *value;
                            }
                        }
                    });
                });
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(locked.is_none(), button("Forget", CORAL, 44.0))
                    .clicked()
                {
                    out.push(Command::ForgetDesk(editing.desk.fingerprint.clone()));
                    close = true;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(primary("Save").min_size(vec2(96.0, 44.0))).clicked() {
                        // A name left as the machine's own is no name of the person's.
                        let name = editing.name.trim();
                        let own = if name == editing.desk.name && editing.desk.own_name.is_empty() {
                            ""
                        } else {
                            name
                        };
                        out.push(Command::EditDesk {
                            fingerprint: editing.desk.fingerprint.clone(),
                            name: own.to_string(),
                            permission: editing.permission,
                            // Set from the card's menu, not here.
                            favorite: editing.desk.favorite,
                        });
                        close = true;
                    }
                    if ui.add(button("Cancel", WHITE, 44.0)).clicked() {
                        close = true;
                    }
                });
            });
        });
        if close {
            self.editing = None;
        }
    }

    /// The desks connected here right now, each with a way to send it away.
    fn connected_dialog(&mut self, ctx: &egui::Context, state: &AppState, out: &mut Vec<Command>) {
        if !self.connected {
            return;
        }
        let mut close = false;
        dialog(ctx, "connected-here", |ui| {
            ui.set_width(400.0);
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.horizontal(|ui| {
                ui.label(RichText::new("Connected here").font(heading(20.0)));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(button("Close", WHITE, 36.0)).clicked() {
                        close = true;
                    }
                });
            });
            if state.guests.is_empty() {
                ui.label(muted("No desk is connected."));
            }
            for guest in &state.guests {
                Frame::new()
                    .stroke(outline())
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(guest_name(state, guest)).font(heading(16.0)),
                                    );
                                    ui.label(
                                        RichText::new(&guest.address).font(mono(12.0)).color(MUTED),
                                    );
                                });
                                ui.label(
                                    RichText::new(format!(
                                        "for {}",
                                        duration(state.now.saturating_sub(guest.since))
                                    ))
                                    .size(13.0),
                                );
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.add(button("Disconnect", CORAL, 40.0)).clicked() {
                                    out.push(Command::EndSession(guest.name.clone()));
                                }
                            });
                        });
                    });
            }
        });
        if close {
            self.connected = false;
        }
    }

    /// Closing ends every connection: said first, with the way to keep them (the tray).
    fn close_dialog(&mut self, ctx: &egui::Context, state: &AppState, out: &mut Vec<Command>) {
        if !self.closing {
            return;
        }
        let mut did = None;
        dialog(ctx, "close-app", |ui| {
            ui.set_width(392.0);
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.label(RichText::new("Close Illumination?").font(heading(22.0)));
            let connections = state.sessions.len() + state.outgoing;
            ui.label(if connections == 1 {
                "1 connection will end.".to_string()
            } else {
                format!("{connections} connections will end.")
            });
            ui.label(muted(
                "This desk cannot be reached until you open it again.",
            ));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.add(button("Cancel", WHITE, 44.0)).clicked() {
                    did = Some(Command::DismissDial);
                }
                if ui.add(button("Minimize to tray", WHITE, 44.0)).clicked() {
                    did = Some(Command::Hide);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(button("Close", CORAL, 44.0)).clicked() {
                        did = Some(Command::Quit);
                    }
                });
            });
        });
        if let Some(command) = did {
            self.closing = false;
            if command != Command::DismissDial {
                out.push(command);
            }
        }
    }

    /// The update: what is new and the way to install it, or how installing goes.
    fn update_dialog(&mut self, ctx: &egui::Context, state: &AppState, out: &mut Vec<Command>) {
        if !self.update {
            return;
        }
        let Some(release) = &state.update.available else {
            self.update = false;
            return;
        };
        let mut close = false;
        dialog(ctx, "update", |ui| {
            ui.set_width(420.0);
            ui.spacing_mut().item_spacing.y = 12.0;
            ui.label(caps("Update").color(MUTED));
            ui.label(
                RichText::new(format!("Illumination {}", release.version)).font(heading(24.0)),
            );
            let on_its_way = match state.update.progress {
                Progress::Downloading(percent) => Some(percent),
                Progress::Installing => Some(Some(100)),
                _ => None,
            };
            if let Some(percent) = on_its_way {
                progress_bar(ui, percent);
                ui.label(muted("Illumination closes and opens again when it is done.").size(13.0));
                if matches!(state.update.progress, Progress::Downloading(_)) {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(button("Cancel", WHITE, 44.0)).clicked() {
                            out.push(Command::CancelUpdate);
                        }
                    });
                }
                return;
            }
            ui.label(
                RichText::new(format!("Current: v{}", state.version))
                    .font(mono(13.0))
                    .color(MUTED),
            );
            release_notes(ui, &release.notes);
            if let Progress::Failed(why) = &state.update.progress {
                note_box(ui, CORAL, &format!("The update did not finish: {why}."));
            }
            let connections = state.sessions.len() + state.outgoing;
            let can_update = state.update.installed && connections == 0;
            if !state.update.installed {
                note_box(
                    ui,
                    CREAM,
                    &format!(
                        "This copy of Illumination is not installed, so it cannot update \
                         itself. Get {} from the release page.",
                        release.version
                    ),
                );
            } else if connections > 0 {
                note_box(ui, CORAL, &busy_sentence(state));
            } else {
                ui.label(muted("Illumination closes and opens again.").size(13.0));
            }
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if state.update.installed {
                        let go = primary("Update now").min_size(vec2(0.0, 44.0));
                        if ui.add_enabled(can_update, go).clicked() {
                            out.push(Command::UpdateNow);
                        }
                    } else if ui
                        .add(primary("Open the release page").min_size(vec2(0.0, 44.0)))
                        .clicked()
                    {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(
                            "https://github.com/Yooouuuuuuu/illumination/releases/latest",
                        ));
                    }
                    if ui.add(button("Later", WHITE, 44.0)).clicked() {
                        close = true;
                    }
                });
            });
        });
        if close {
            self.update = false;
        }
    }

    fn leave_dialog(&mut self, ctx: &egui::Context, out: &mut Vec<Command>) {
        if !self.leaving {
            return;
        }
        let mut did = None;
        dialog(ctx, "leave-settings", |ui| {
            ui.set_width(360.0);
            ui.spacing_mut().item_spacing.y = 12.0;
            ui.label(RichText::new("Save the changes?").font(heading(22.0)));
            ui.horizontal(|ui| {
                if ui.add(button("Cancel", WHITE, 44.0)).clicked() {
                    did = Some(0);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(primary("Save").min_size(vec2(96.0, 44.0))).clicked() {
                        did = Some(2);
                    }
                    if ui.add(button("Discard", WHITE, 44.0)).clicked() {
                        did = Some(1);
                    }
                });
            });
        });
        match did {
            Some(0) => self.leaving = false,
            Some(1) => self.leave(),
            Some(2) => {
                if let Some(pending) = self.pending.take() {
                    out.push(Command::SaveSettings(pending));
                }
                self.leave();
            }
            _ => {}
        }
    }

    fn settings(&mut self, ui: &mut Ui, state: &AppState, out: &mut Vec<Command>) {
        let Some(mut s) = self.pending.clone() else {
            return;
        };
        let changed = unsaved(&state.settings, &s);
        let full = ui.available_width();
        let width = (full - 64.0).clamp(280.0, 1056.0);
        let side = ((full - width) / 2.0).max(0.0);
        let menu_width = if width >= 700.0 { 190.0 } else { 0.0 };
        ui.add_space(24.0);
        let height = ui.available_height();
        ui.horizontal_top(|ui| {
            ui.add_space(side);
            ui.spacing_mut().item_spacing.x = 24.0;
            if menu_width > 0.0 {
                ui.allocate_ui_with_layout(
                    vec2(menu_width, height),
                    Layout::top_down(Align::Min),
                    |ui| self.menu(ui, &changed),
                );
            }
            let rest = width - menu_width - if menu_width > 0.0 { 24.0 } else { 0.0 };
            ui.allocate_ui_with_layout(vec2(rest, height), Layout::top_down(Align::Min), |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        ui.set_width(rest - 14.0);
                        ui.spacing_mut().item_spacing.y = 14.0;
                        self.sections(ui, state, &mut s, out);
                        ui.add_space(24.0);
                    });
            });
        });
        self.pending = Some(s);
    }

    /// The sections; a click brings one into view, a dot marks an unsaved change in it.
    fn menu(&mut self, ui: &mut Ui, changed: &[Item]) {
        Frame::new()
            .fill(WHITE)
            .stroke(outline())
            .corner_radius(CornerRadius::same(16))
            .inner_margin(Margin::same(8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 2.0;
                for (section, items) in MENU {
                    let text = if items.iter().any(|item| changed.contains(item)) {
                        format!("{section}  •")
                    } else {
                        section.to_string()
                    };
                    let head = egui::Button::new(RichText::new(text).font(heading(17.0)))
                        .fill(WHITE)
                        .stroke(Stroke::NONE)
                        .min_size(vec2(ui.available_width(), 46.0));
                    if ui.add(head).clicked() {
                        self.jump = Some(items[0]);
                    }
                }
            });
    }

    /// Marks where a setting is drawn, and brings it into view when the menu asked for it.
    fn here(&mut self, ui: &mut Ui, item: Item) {
        if self.jump == Some(item) {
            let (_, mark) = ui.allocate_exact_size(vec2(1.0, 0.0), Sense::hover());
            mark.scroll_to_me(Some(Align::TOP));
            self.jump = None;
        }
    }

    fn sections(
        &mut self,
        ui: &mut Ui,
        state: &AppState,
        s: &mut Settings,
        out: &mut Vec<Command>,
    ) {
        let title = |ui: &mut Ui, text: &str| {
            ui.label(RichText::new(text).font(heading(18.0)));
        };
        let label = |ui: &mut Ui, text: &str| {
            ui.label(RichText::new(text).font(semibold(14.0)));
        };
        card(ui, WHITE, |ui| {
            ui.set_width(ui.available_width());
            self.here(ui, Item::StartOnBoot);
            title(ui, "General");
            ui.checkbox(&mut s.start_on_boot, "Start on boot");
            self.here(ui, Item::Key);
            label(ui, "This desk's key");
            ui.horizontal(|ui| key_box(ui, &state.fingerprint, TILE, 15.0));
            ui.label(muted("Shown to other desks the first time they connect."));
            self.here(ui, Item::Logs);
            ui.checkbox(&mut s.detailed_logs, "Detailed logs");
            ui.label(muted(
                "Records more detail, for troubleshooting. Logs are kept for 7 days.",
            ));
        });
        card(ui, WHITE, |ui| {
            ui.set_width(ui.available_width());
            self.here(ui, Item::Limits);
            ui.horizontal(|ui| {
                title(ui, "Connection");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(button("Reset to defaults", WHITE, 36.0)).clicked() {
                        *s = s.with_outgoing_reset();
                    }
                });
            });
            ui.label(muted("When I connect to another desk."));
            egui::Grid::new("outgoing")
                .num_columns(2)
                .spacing(vec2(16.0, 10.0))
                .show(ui, |ui| {
                    // The two numbers are typed: no dragging, and the pointer says so.
                    label(ui, "Max bitrate");
                    ui.add(
                        egui::DragValue::new(&mut s.max_bitrate_mbps)
                            .range(1.0..=200.0)
                            .speed(0.0)
                            .max_decimals(1)
                            .suffix(" Mb/s"),
                    )
                    .on_hover_cursor(egui::CursorIcon::Text);
                    ui.end_row();
                    label(ui, "Max frame rate");
                    egui::ComboBox::from_id_salt("fps")
                        .selected_text(format!("{} fps", s.max_fps))
                        .show_ui(ui, |ui| {
                            for fps in [15, 30, 60, 120] {
                                ui.selectable_value(&mut s.max_fps, fps, format!("{fps} fps"));
                            }
                        });
                    ui.end_row();
                    label(ui, "Delay limit");
                    ui.add(
                        egui::DragValue::new(&mut s.latency_limit_ms)
                            .range(10..=500)
                            .speed(0.0)
                            .suffix(" ms"),
                    )
                    .on_hover_cursor(egui::CursorIcon::Text);
                    ui.end_row();
                });
            self.here(ui, Item::Sound);
            ui.checkbox(&mut s.sound, "Play sound");
            self.here(ui, Item::Clipboard);
            ui.checkbox(&mut s.clipboard, "Share clipboard");
            self.here(ui, Item::Reconnect);
            ui.checkbox(&mut s.reconnect, "Reconnect automatically");
            self.here(ui, Item::Shrink);
            ui.checkbox(&mut s.allow_scale, "Shrink picture when slow");
            self.here(ui, Item::Tab);
            ui.horizontal(|ui| {
                // Typed, as the numbers above.
                label(ui, "Exit button transparency");
                ui.add(
                    egui::DragValue::new(&mut s.exit_tab_transparency)
                        .range(0..=100)
                        .speed(0.0)
                        .suffix(" %"),
                )
                .on_hover_cursor(egui::CursorIcon::Text);
            });
        });
        // Who may connect is not changed while a desk is connected: it could let itself in.
        let locked = lock_note(state);
        if locked.is_some() {
            *s = s.with_incoming_of(&state.settings);
        }
        card(ui, WHITE, |ui| {
            ui.set_width(ui.available_width());
            self.here(ui, Item::Permission);
            title(ui, "Incoming");
            ui.label(muted("When another desk connects here."));
            if let Some(note) = &locked {
                ui.label(RichText::new(note).font(semibold(14.0)));
            }
            ui.add_enabled_ui(locked.is_none(), |ui| {
                label(ui, "When a new desk connects");
                ui.horizontal(|ui| {
                    let each = vec2(150.0, 44.0);
                    for permission in [Permission::Always, Permission::Ask, Permission::Never] {
                        let (word, fill) = permission_look(permission);
                        if choice(ui, word, s.new_desks == permission, fill, each) {
                            s.new_desks = permission;
                        }
                    }
                });
                if s.new_desks == Permission::Always {
                    ui.label(
                        RichText::new("Any desk that can reach this one connects without asking.")
                            .font(semibold(14.0)),
                    );
                }
                ui.label(muted("What you set for one desk overrides this."));
                self.here(ui, Item::Actions);
                label(ui, "Allowed actions");
                let mut always = true;
                ui.add_enabled(false, egui::Checkbox::new(&mut always, "See screen"));
                ui.checkbox(&mut s.let_control, "Use keyboard and mouse");
                ui.checkbox(&mut s.let_sound, "Hear sound");
                ui.checkbox(&mut s.let_clipboard, "Share clipboard");
                self.here(ui, Item::Idle);
                ui.horizontal(|ui| {
                    label(ui, "Disconnect when idle for");
                    let name = |minutes: u32| match minutes {
                        0 => "Never".to_string(),
                        60 => "1 hour".to_string(),
                        120 => "2 hours".to_string(),
                        m => format!("{m} minutes"),
                    };
                    egui::ComboBox::from_id_salt("idle")
                        .selected_text(name(s.idle_minutes))
                        .show_ui(ui, |ui| {
                            for minutes in [10, 30, 60, 120, 0] {
                                ui.selectable_value(&mut s.idle_minutes, minutes, name(minutes));
                            }
                        });
                });
            });
        });
        card(ui, WHITE, |ui| {
            ui.set_width(ui.available_width());
            self.here(ui, Item::Trusted);
            title(ui, "Trusted desks");
            ui.label(muted("These desks connect without asking."));
            let mut trusted: Vec<&Desk> = state
                .desks
                .iter()
                .filter(|d| d.permission == Some(Permission::Always))
                .collect();
            trusted.sort_by_key(|d| d.name.to_lowercase());
            if trusted.is_empty() {
                ui.label(muted(
                    "None yet. Answer \"Always\" when a desk asks, or set it in the desk's Edit.",
                ));
            }
            for desk in trusted {
                Frame::new()
                    .stroke(outline())
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(&desk.name).font(heading(16.0)));
                                    ui.label(
                                        RichText::new(&desk.address).font(mono(12.0)).color(MUTED),
                                    );
                                });
                                let last = if desk.last_in > 0 {
                                    format!("Connected here {}", day(state.now, desk.last_in))
                                } else {
                                    "Has not connected here yet".to_string()
                                };
                                ui.label(RichText::new(last).size(13.0));
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                // Back to what Settings says for a new desk; at once, as in
                                // the desk's Edit.
                                if ui
                                    .add_enabled(locked.is_none(), button("Remove", WHITE, 36.0))
                                    .clicked()
                                {
                                    out.push(Command::EditDesk {
                                        fingerprint: desk.fingerprint.clone(),
                                        name: desk.own_name.clone(),
                                        permission: None,
                                        favorite: desk.favorite,
                                    });
                                }
                            });
                        });
                    });
            }
        });
        card(ui, WHITE, |ui| {
            ui.set_width(ui.available_width());
            self.here(ui, Item::About);
            ui.horizontal(|ui| {
                title(ui, "About");
                ui.label(muted(format!(
                    "Illumination {} · GPL-3.0-only",
                    state.version
                )));
                ui.hyperlink_to(
                    RichText::new("Source code").size(14.0),
                    "https://github.com/Yooouuuuuuu/illumination",
                );
            });
            ui.add_space(6.0);
            match update_line(ui, state) {
                Some(true) => self.update = true,
                Some(false) => out.push(Command::CheckUpdates),
                None => {}
            }
        });
    }
}

/// A dialog over the window, in the app's frame.
fn dialog(ctx: &egui::Context, id: &str, add: impl FnOnce(&mut Ui)) {
    let frame = Frame::new()
        .fill(WHITE)
        .stroke(outline())
        .corner_radius(CornerRadius::same(16))
        .inner_margin(Margin::same(24));
    egui::Modal::new(egui::Id::unique(id))
        .frame(frame)
        .backdrop_color(Color32::from_rgba_unmultiplied(0x1E, 0x2A, 0x4A, 150))
        .show(ctx, add);
}

/// How long, as a clock: 12:40, 1:03:15.
pub fn duration(seconds: u64) -> String {
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// What a connected desk is called here: your own name for it when you gave one (the desk at
/// that address), else the name it gave itself.
pub fn guest_name(state: &AppState, guest: &Guest) -> String {
    state
        .desks
        .iter()
        .find(|d| d.address == guest.address && !d.own_name.is_empty())
        .map_or_else(|| guest.name.clone(), |d| d.own_name.clone())
}

/// The desks connected here, by the names shown for them.
fn connected_names(state: &AppState) -> Vec<String> {
    if state.guests.len() == state.sessions.len() {
        state.guests.iter().map(|g| guest_name(state, g)).collect()
    } else {
        state.sessions.clone()
    }
}

/// Why who may connect cannot be changed now: a desk is connected here. `None`: it can.
fn lock_note(state: &AppState) -> Option<String> {
    let who = match connected_names(state).len() {
        0 => return None,
        1 => "1 desk is".to_string(),
        n => format!("{n} desks are"),
    };
    Some(format!(
        "Locked while {who} connected. This keeps a connected desk from changing who may \
         connect here."
    ))
}

/// This desk's state, top right. Returns whether it was clicked while a desk is connected
/// (which opens the list of them).
fn status_pill(ui: &mut Ui, state: &AppState) -> bool {
    // The number only, as the tray says it: the "Connected here" list has the names.
    let names = connected_names(state);
    let (text, fill, solid) = match (&state.incoming, names.as_slice()) {
        (Incoming::Failed(_), _) => ("Cannot be reached".to_string(), CORAL, false),
        (_, [_]) => ("1 desk connected here".to_string(), YELLOW, true),
        (_, [_, ..]) => (
            format!("{} desks connected here", names.len()),
            YELLOW,
            true,
        ),
        (Incoming::Listening, []) if state.settings.new_desks == Permission::Never => {
            ("New desks are refused".to_string(), WHITE, false)
        }
        (Incoming::Listening, []) if state.settings.new_desks == Permission::Always => {
            ("Open to any desk".to_string(), GREEN, true)
        }
        (Incoming::Listening, []) => ("Ready for connections".to_string(), GREEN, true),
        (Incoming::Off, []) => ("Not listening".to_string(), WHITE, false),
    };
    pill(ui, text, fill, solid).clicked() && !state.sessions.is_empty()
}

/// The session windows open to other desks, beside the status: their number, never a name
/// (each window says which desk it shows). Nothing when there are none. Returns whether it
/// was clicked, which brings them to the front.
fn outgoing_pill(ui: &mut Ui, state: &AppState) -> bool {
    let text = match state.outgoing {
        0 => return false,
        1 => "Connected to 1 desk".to_string(),
        n => format!("Connected to {n} desks"),
    };
    // Blue, as for things on their way: the yellow of a desk connected here is the one to
    // watch.
    pill(ui, text, BLUE, true)
        .on_hover_text("Bring the session windows to the front")
        .clicked()
}

/// A pill of the top bar: a dot, filled or hollow, and a few words.
fn pill(ui: &mut Ui, text: String, fill: Color32, solid: bool) -> egui::Response {
    // Painted, not laid out: no taller than its words (smaller than the button beside it)
    // and level with that button.
    let words = ui.painter().layout_no_wrap(text, semibold(13.0), NAVY);
    let size = vec2(12.0 + 10.0 + 8.0 + words.size().x + 12.0, 28.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    p.rect(
        rect,
        CornerRadius::same(14),
        fill,
        outline(),
        StrokeKind::Inside,
    );
    // Filled when open, hollow when not: told apart without colour.
    let dot = egui::pos2(rect.left() + 17.0, rect.center().y);
    if solid {
        p.circle_filled(dot, 5.0, NAVY);
    } else {
        p.circle_stroke(dot, 4.0, Stroke::new(2.0, NAVY));
    }
    p.galley(
        egui::pos2(rect.left() + 30.0, rect.center().y - words.size().y / 2.0),
        words,
        NAVY,
    );
    response
}

/// The update, top right beside the status: strong yellow while it waits for the person,
/// blue while it is on its way. Nothing when there is none. Returns whether it was clicked.
fn update_pill(ui: &mut Ui, state: &AppState) -> bool {
    let Some(release) = &state.update.available else {
        return false;
    };
    let (fill, text, percent) = match state.update.progress {
        Progress::Downloading(percent) => (
            BLUE,
            format!("Downloading {}", release.version),
            Some(percent),
        ),
        Progress::Installing => (BLUE, format!("Installing {}", release.version), None),
        _ => (YELLOW, format!("Update to {}", release.version), None),
    };
    let waiting = fill == YELLOW;
    let words = ui.painter().layout_no_wrap(text, semibold(13.0), NAVY);
    let tail = percent.map(|p| {
        let shown = p.map_or_else(|| "…".to_string(), |p| format!("{p} %"));
        ui.painter().layout_no_wrap(shown, semibold(13.0), NAVY)
    });
    const BAR: f32 = 56.0;
    let icon = if waiting { 14.0 + 8.0 } else { 0.0 };
    let tail_width = tail.as_ref().map_or(0.0, |t| 8.0 + BAR + 8.0 + t.size().x);
    let size = vec2(10.0 + icon + words.size().x + tail_width + 12.0, 28.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    p.rect(
        rect,
        CornerRadius::same(14),
        fill,
        outline(),
        StrokeKind::Inside,
    );
    let mut x = rect.left() + 10.0;
    let y = rect.center().y;
    if waiting {
        // A download arrow over a line.
        let c = x + 7.0;
        let line = Stroke::new(2.2, NAVY);
        p.line_segment([egui::pos2(c, y - 6.0), egui::pos2(c, y + 2.5)], line);
        p.line_segment([egui::pos2(c - 4.0, y - 1.5), egui::pos2(c, y + 2.5)], line);
        p.line_segment([egui::pos2(c + 4.0, y - 1.5), egui::pos2(c, y + 2.5)], line);
        p.line_segment(
            [egui::pos2(c - 6.0, y + 6.5), egui::pos2(c + 6.0, y + 6.5)],
            line,
        );
        x += icon;
    }
    let width = words.size().x;
    p.galley(egui::pos2(x, y - words.size().y / 2.0), words, NAVY);
    x += width;
    if let (Some(percent), Some(tail)) = (percent, tail) {
        x += 8.0;
        let track = egui::Rect::from_min_size(egui::pos2(x, y - 3.0), vec2(BAR, 6.0));
        p.rect(
            track,
            CornerRadius::same(3),
            WHITE,
            Stroke::new(1.5, NAVY),
            StrokeKind::Inside,
        );
        let done = track.width() * f32::from(percent.unwrap_or(0)) / 100.0;
        p.with_clip_rect(egui::Rect::from_min_size(track.min, vec2(done, 6.0)))
            .rect_filled(track, CornerRadius::same(3), NAVY);
        x += BAR + 8.0;
        p.galley(egui::pos2(x, y - tail.size().y / 2.0), tail, NAVY);
    }
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// How far the update is, with the percentage inside the bar.
fn progress_bar(ui: &mut Ui, percent: Option<u8>) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
    let p = ui.painter();
    let round = CornerRadius::same(14);
    p.rect_filled(rect, round, CREAM);
    if let Some(percent) = percent {
        let done = rect.width() * f32::from(percent.min(100)) / 100.0;
        let filled = egui::Rect::from_min_size(rect.min, vec2(done, rect.height()));
        p.with_clip_rect(filled).rect_filled(rect, round, YELLOW);
        if percent > 0 && percent < 100 {
            let x = rect.left() + done;
            p.with_clip_rect(rect.shrink(1.0)).line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                Stroke::new(OUTLINE, NAVY),
            );
        }
    }
    p.rect_stroke(rect, round, outline(), StrokeKind::Inside);
    let shown = percent.map_or_else(|| "…".to_string(), |p| format!("{p} %"));
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        shown,
        mono(13.0),
        NAVY,
    );
}

/// A release's notes, as its changelog entry says them. A long entry scrolls.
fn release_notes(ui: &mut Ui, notes: &str) {
    let lines = note_lines(notes);
    if lines.is_empty() {
        return;
    }
    egui::ScrollArea::vertical()
        .max_height(280.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            for line in lines {
                match line {
                    NoteLine::Heading(text) => {
                        ui.add_space(2.0);
                        ui.label(caps(&text).size(12.0).color(MUTED));
                    }
                    NoteLine::Bullet(text) => {
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            ui.label(RichText::new("•").size(14.0));
                            ui.add(egui::Label::new(with_bold(&text)).wrap());
                        });
                    }
                    NoteLine::Text(text) => {
                        ui.add(egui::Label::new(with_bold(&text)).wrap());
                    }
                }
            }
        });
}

/// A line of notes with its `**bold**` parts in semibold.
fn with_bold(text: &str) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    for (i, part) in text.split("**").enumerate() {
        if part.is_empty() {
            continue;
        }
        let font = if i % 2 == 1 {
            semibold(14.0)
        } else {
            FontId::proportional(14.0)
        };
        job.append(
            part,
            0.0,
            egui::TextFormat {
                font_id: font,
                color: NAVY,
                ..Default::default()
            },
        );
    }
    job
}

/// A boxed line in a dialog: coral for what stands in the way, cream for a note.
fn note_box(ui: &mut Ui, fill: Color32, text: &str) {
    Frame::new()
        .fill(fill)
        .stroke(outline())
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).size(14.0));
        });
}

/// Why an update has to wait: what is connected, in words.
fn busy_sentence(state: &AppState) -> String {
    let mut parts = Vec::new();
    match connected_names(state).len() {
        0 => {}
        1 => parts.push("1 desk is connected here".to_string()),
        n => parts.push(format!("{n} desks are connected here")),
    }
    match state.outgoing {
        0 => {}
        1 => parts.push("1 session window is open".to_string()),
        n => parts.push(format!("{n} session windows are open")),
    }
    format!(
        "Updating ends all connections. {}. Disconnect them first.",
        parts.join(" and ")
    )
}

/// The update line of About: what the last check found, and the button for what comes
/// next. `Some(true)`: open the update; `Some(false)`: check now.
fn update_line(ui: &mut Ui, state: &AppState) -> Option<bool> {
    let mut asked = None;
    ui.horizontal(|ui| {
        let update = &state.update;
        if let Some(release) = &update.available {
            ui.label(
                RichText::new(format!("{} is available.", release.version)).font(semibold(14.0)),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(button("Update now", YELLOW, 36.0)).clicked() {
                    asked = Some(true);
                }
            });
            return;
        }
        match &update.checked {
            Checked::Not => {
                ui.label(muted("Not checked for updates yet."));
            }
            Checked::Checking => {
                ui.label(muted("Checking for updates…"));
            }
            Checked::Done(at) => {
                check_mark(ui, true);
                ui.label(RichText::new("Up to date.").font(semibold(14.0)));
                ui.label(muted(format!("Checked {}.", day(state.now, *at))));
            }
            Checked::Failed(why) => {
                check_mark(ui, false);
                ui.label(RichText::new("Could not check for updates.").font(semibold(14.0)));
                ui.label(muted(sentence(why)));
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let idle = update.checked != Checked::Checking;
            if ui
                .add_enabled(idle, button("Check for updates", WHITE, 36.0))
                .clicked()
            {
                asked = Some(false);
            }
        });
    });
    asked
}

/// A round mark: a green tick for done, a coral cross for failed.
fn check_mark(ui: &mut Ui, done: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
    let p = ui.painter();
    let c = rect.center();
    p.circle(
        c,
        9.0,
        if done { GREEN } else { CORAL },
        Stroke::new(OUTLINE, NAVY),
    );
    let line = Stroke::new(2.0, NAVY);
    if done {
        p.line_segment([c + vec2(-4.0, 0.0), c + vec2(-1.0, 3.0)], line);
        p.line_segment([c + vec2(-1.0, 3.0), c + vec2(4.5, -3.0)], line);
    } else {
        p.line_segment([c + vec2(-3.5, -3.5), c + vec2(3.5, 3.5)], line);
        p.line_segment([c + vec2(3.5, -3.5), c + vec2(-3.5, 3.5)], line);
    }
}

/// Words as a sentence: a capital first, a full stop last.
fn sentence(text: &str) -> String {
    let mut chars = text.trim().chars();
    let mut out: String = chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    if !out.ends_with('.') {
        out.push('.');
    }
    out
}

/// This desk: the address other desks dial, with a way to copy it, and what happens to a new
/// desk that connects. Returns whether that line was clicked (which opens Settings at
/// Incoming).
fn this_desk(ui: &mut Ui, state: &AppState, width: f32, min_height: f32) -> bool {
    let mut to_incoming = false;
    card(ui, SOFT, |ui| {
        ui.set_width(width - 52.0);
        ui.set_min_height(min_height);
        ui.label(caps("This desk"));
        match state.addresses.first() {
            Some(address) => {
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if copy_button(ui) {
                            ui.ctx().copy_text(address.clone());
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(RichText::new(address).font(mono(32.0)))
                                    .truncate(),
                            );
                        });
                    });
                });
            }
            None => {
                ui.label(RichText::new("No tailnet address").font(heading(22.0)));
                ui.label(
                    RichText::new("Start Tailscale so other desks can reach this one.").size(14.0),
                );
            }
        }
        if let Incoming::Failed(why) = &state.incoming {
            ui.label(RichText::new(format!("Cannot be reached: {why}")).font(semibold(14.0)));
        }
        to_incoming = new_desks_line(ui, state);
    });
    to_incoming
}

/// Copies this desk's address: two sheets, one behind the other. Returns whether it was
/// clicked.
fn copy_button(ui: &mut Ui) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(40.0, 36.0), Sense::click());
    let fill = if response.is_pointer_button_down_on() {
        YELLOW
    } else if response.hovered() {
        CREAM
    } else {
        WHITE
    };
    let p = ui.painter();
    p.rect(
        rect,
        CornerRadius::same(10),
        fill,
        outline(),
        StrokeKind::Inside,
    );
    let (c, line) = (rect.center(), Stroke::new(2.0, NAVY));
    let sheet = |at: Vec2| egui::Rect::from_center_size(c + at, vec2(11.0, 12.0));
    let round = CornerRadius::same(3);
    p.rect_stroke(sheet(vec2(-2.5, -2.5)), round, line, StrokeKind::Middle);
    p.rect(sheet(vec2(2.5, 2.5)), round, fill, line, StrokeKind::Middle);
    response.on_hover_text("Copy the address").clicked()
}

/// What happens to a desk that connects here for the first time, and how many desks are
/// always allowed: one line, in the words and with the mark of that permission. Returns
/// whether it was clicked.
fn new_desks_line(ui: &mut Ui, state: &AppState) -> bool {
    let rule = state.settings.new_desks;
    let trusted = state
        .desks
        .iter()
        .filter(|d| d.permission == Some(Permission::Always))
        .count();
    let first = match rule {
        Permission::Always => "Any desk can connect without a question",
        Permission::Ask => "New desks are asked first",
        Permission::Never => "New desks are refused",
    };
    // With every desk let in, the ones always allowed are nothing apart.
    let rest = match (rule, trusted) {
        (Permission::Always, _) | (_, 0) => String::new(),
        (_, 1) => " · 1 desk always allowed".to_string(),
        (_, n) => format!(" · {n} desks always allowed"),
    };
    let mut job = egui::text::LayoutJob::default();
    job.append(first, 0.0, egui::TextFormat::simple(semibold(14.0), NAVY));
    job.append(
        &rest,
        0.0,
        egui::TextFormat::simple(FontId::proportional(14.0), NAVY),
    );
    job.wrap.max_width = (ui.available_width() - 28.0 - 18.0).max(60.0);
    let words = ui.painter().layout_job(job);
    let size = vec2(28.0 + words.size().x + 18.0, words.size().y.max(22.0));
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    let mark = egui::pos2(rect.left() + 10.0, rect.center().y);
    let (_, fill) = permission_look(rule);
    p.circle(mark, 9.0, fill, outline());
    permission_sign(p, mark, rule);
    let text = egui::pos2(rect.left() + 28.0, rect.center().y - words.size().y / 2.0);
    let (wide, tall) = (words.size().x, words.size().y);
    p.galley(text, words, NAVY);
    let (tip, line) = (
        egui::pos2(rect.right() - 4.0, rect.center().y),
        Stroke::new(2.0, NAVY),
    );
    p.line_segment([tip + vec2(-4.0, -4.0), tip], line);
    p.line_segment([tip + vec2(-4.0, 4.0), tip], line);
    if response.hovered() {
        let y = text.y + tall + 1.0;
        p.line_segment(
            [egui::pos2(text.x, y), egui::pos2(text.x + wide, y)],
            Stroke::new(1.0, NAVY),
        );
    }
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

enum OnCard {
    Connect,
    Edit,
    /// Add it to the favourites, or take it out.
    Favorite,
}

/// A desk as a card. Above: its system and whether it answers, its name, where it is, when
/// it was last connected. Below, on a strip of its own: its permission in words, and the
/// button that opens what can be done with it. A double click connects to it. A favourite is
/// cream instead of white.
fn desk_card(
    ui: &mut Ui,
    desk: &Desk,
    state: &AppState,
    tab: Tab,
    width: f32,
    dialing: bool,
) -> Option<OnCard> {
    let mut did = None;
    let can_connect = desk.connect_to.is_some();
    let whole = ui.scope_builder(
        egui::UiBuilder::new()
            .id_salt(("desk", &desk.fingerprint))
            .sense(Sense::click()),
        |ui| {
            Frame::new()
                .fill(if desk.favorite { CREAM } else { WHITE })
                .stroke(outline())
                .corner_radius(CornerRadius::same(16))
                .show(ui, |ui| {
                    ui.set_width(width - 2.0 * OUTLINE);
                    ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
                    // Words that could be selected would take the clicks meant for the card.
                    ui.style_mut().interaction.selectable_labels = false;
                    Frame::new()
                        .inner_margin(Margin::symmetric(16, 12))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.spacing_mut().item_spacing.y = 4.0;
                            ui.spacing_mut().interact_size.y = 20.0;
                            ui.horizontal(|ui| {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if let Some(target) = &desk.connect_to
                                        && let Some(on) = state.online.get(target)
                                    {
                                        online_mark(ui, *on);
                                    }
                                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                        let os = if desk.os.is_empty() { " " } else { &desk.os };
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(os.to_uppercase())
                                                    .font(heading(12.0))
                                                    .extra_letter_spacing(1.0)
                                                    .color(MUTED),
                                            )
                                            .truncate(),
                                        );
                                    });
                                });
                            });
                            let line = |ui: &mut Ui, text: RichText| {
                                ui.add(egui::Label::new(text).truncate());
                            };
                            line(ui, RichText::new(&desk.name).font(heading(20.0)));
                            line(
                                ui,
                                RichText::new(&desk.address).font(mono(13.0)).color(MUTED),
                            );
                            // (A card is narrow: the date goes without its "on".)
                            let day = |then| {
                                let day = day(state.now, then);
                                day.strip_prefix("on ").unwrap_or(&day).to_string()
                            };
                            let last = match tab {
                                Tab::CameIn
                                    if desk.last_in >= desk.tried_in && desk.last_in > 0 =>
                                {
                                    format!("Connected here {}", day(desk.last_in))
                                }
                                Tab::CameIn => {
                                    format!("Tried to connect {}", day(desk.tried_in))
                                }
                                _ if desk.last_out > 0 => {
                                    format!("Connected {}", day(desk.last_out))
                                }
                                _ if desk.last_in > 0 => {
                                    format!("Connected here {}", day(desk.last_in))
                                }
                                _ => "Not connected yet".to_string(),
                            };
                            line(ui, RichText::new(last).size(13.0).color(MUTED));
                        });
                    let strip = Frame::new()
                        .fill(STRIP)
                        .corner_radius(CornerRadius {
                            nw: 0,
                            ne: 0,
                            sw: 14,
                            se: 14,
                        })
                        .inner_margin(Margin {
                            left: 16,
                            right: 10,
                            top: 8,
                            bottom: 8,
                        })
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    let more = more_button(ui);
                                    egui::Popup::menu(&more)
                                        .frame(
                                            Frame::new()
                                                .fill(WHITE)
                                                .stroke(outline())
                                                .corner_radius(CornerRadius::same(12))
                                                .inner_margin(Margin::same(6)),
                                        )
                                        .show(|ui| {
                                            ui.spacing_mut().item_spacing.y = 2.0;
                                            let entry = |text: &str| {
                                                button(text, WHITE, 36.0)
                                                    .stroke(Stroke::NONE)
                                                    .min_size(vec2(196.0, 36.0))
                                            };
                                            if can_connect
                                                && ui
                                                    .add_enabled(!dialing, entry("Connect"))
                                                    .clicked()
                                            {
                                                did = Some(OnCard::Connect);
                                            }
                                            if ui.add(entry("Edit")).clicked() {
                                                did = Some(OnCard::Edit);
                                            }
                                            let favorite = if desk.favorite {
                                                "Remove from favorites"
                                            } else {
                                                "Add to favorites"
                                            };
                                            if ui.add(entry(favorite)).clicked() {
                                                did = Some(OnCard::Favorite);
                                            }
                                        });
                                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                        // Without a permission of its own the desk follows
                                        // Settings.
                                        match desk.permission {
                                            Some(permission) => permission_chip(ui, permission),
                                            None => {
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new("Default permission")
                                                            .size(12.0)
                                                            .color(MUTED),
                                                    )
                                                    .truncate(),
                                                );
                                            }
                                        }
                                    });
                                });
                            });
                        });
                    let top = strip.response.rect;
                    ui.painter().hline(top.x_range(), top.top(), outline());
                });
        },
    );
    if can_connect && !dialing {
        // The whole card takes the double click, so it answers the pointer as a whole: a
        // heavier outline, since a fill would read as a favourite.
        if whole.response.contains_pointer() {
            ui.painter().rect_stroke(
                whole.response.rect,
                CornerRadius::same(16),
                Stroke::new(2.0 * OUTLINE, NAVY),
                StrokeKind::Inside,
            );
        }
        if whole.response.double_clicked() {
            did = Some(OnCard::Connect);
        }
    }
    did
}

/// The button at the bottom right of a card: opens what can be done with the desk.
fn more_button(ui: &mut Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(40.0, 30.0), Sense::click());
    let fill = if response.is_pointer_button_down_on() {
        YELLOW
    } else if response.hovered() {
        SOFT
    } else {
        WHITE
    };
    let p = ui.painter();
    p.rect(
        rect,
        CornerRadius::same(10),
        fill,
        outline(),
        StrokeKind::Inside,
    );
    let (c, line) = (rect.center(), Stroke::new(2.0, NAVY));
    p.line_segment([c + vec2(-5.0, -2.5), c + vec2(0.0, 2.5)], line);
    p.line_segment([c + vec2(0.0, 2.5), c + vec2(5.0, -2.5)], line);
    response
}

/// Whether a desk answers right now: a dot and a word, the dot filled when it does (told
/// apart without colour).
fn online_mark(ui: &mut Ui, on: bool) {
    let (word, color) = if on {
        ("Online", NAVY)
    } else {
        ("Offline", MUTED)
    };
    let words = ui
        .painter()
        .layout_no_wrap(word.to_string(), semibold(13.0), color);
    let (rect, _) = ui.allocate_exact_size(vec2(16.0 + words.size().x, 20.0), Sense::hover());
    let p = ui.painter();
    let dot = egui::pos2(rect.left() + 5.0, rect.center().y);
    if on {
        p.circle_filled(dot, 5.0, color);
    } else {
        p.circle_stroke(dot, 4.0, Stroke::new(2.0, color));
    }
    p.galley(
        egui::pos2(rect.left() + 16.0, rect.center().y - words.size().y / 2.0),
        words,
        color,
    );
}

/// A desk's own permission on its card: the sign and the words, on the colour that
/// permission has everywhere.
fn permission_chip(ui: &mut Ui, permission: Permission) {
    let (word, fill) = permission_look(permission);
    let words = ui
        .painter()
        .layout_no_wrap(word.to_string(), semibold(12.0), NAVY);
    let size = vec2(9.0 + 12.0 + 6.0 + words.size().x + 9.0, 26.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter();
    p.rect(
        rect,
        CornerRadius::same(8),
        fill,
        outline(),
        StrokeKind::Inside,
    );
    permission_sign(
        p,
        egui::pos2(rect.left() + 15.0, rect.center().y),
        permission,
    );
    p.galley(
        egui::pos2(rect.left() + 27.0, rect.center().y - words.size().y / 2.0),
        words,
        NAVY,
    );
}

/// The sign of a permission: a tick for always, a question mark for ask, a cross for never.
fn permission_sign(p: &egui::Painter, c: egui::Pos2, permission: Permission) {
    let line = Stroke::new(2.0, NAVY);
    match permission {
        Permission::Always => {
            p.line_segment([c + vec2(-4.5, 0.5), c + vec2(-1.5, 3.5)], line);
            p.line_segment([c + vec2(-1.5, 3.5), c + vec2(4.5, -3.0)], line);
        }
        Permission::Ask => {
            p.text(c, egui::Align2::CENTER_CENTER, "?", semibold(13.0), NAVY);
        }
        Permission::Never => {
            p.line_segment([c + vec2(-3.5, -3.5), c + vec2(3.5, 3.5)], line);
            p.line_segment([c + vec2(-3.5, 3.5), c + vec2(3.5, -3.5)], line);
        }
    }
}

/// Draws nothing but the first waiting question, on the app's ground: the window of
/// `illum ask`, which the installed host opens for a new desk.
pub fn draw_question(ui: &mut Ui, state: &AppState) -> Vec<Command> {
    let mut out = Vec::new();
    egui::CentralPanel::default()
        .frame(Frame::new().fill(TILE))
        .show(ui, |_| {});
    if let Some(q) = state.questions.front() {
        question(ui.ctx(), q, &mut out);
    }
    out
}

/// A desk wants to connect here: who it is, its key, and the four answers.
fn question(ctx: &egui::Context, q: &Question, out: &mut Vec<Command>) {
    let frame = Frame::new()
        .fill(WHITE)
        .stroke(outline())
        .corner_radius(CornerRadius::same(16))
        .inner_margin(Margin::same(24));
    egui::Modal::new(egui::Id::unique(("question", q.id)))
        .frame(frame)
        .backdrop_color(Color32::from_rgba_unmultiplied(0x1E, 0x2A, 0x4A, 150))
        .show(ctx, |ui| {
            ui.set_width(392.0);
            ui.spacing_mut().item_spacing.y = 8.0;
            let small = if q.os.is_empty() {
                "New desk".to_string()
            } else {
                format!("New desk · {}", q.os)
            };
            ui.label(caps(&small).color(MUTED));
            ui.label(RichText::new(&q.name).font(heading(24.0)));
            ui.label(RichText::new(&q.address).font(mono(14.0)).color(MUTED));
            ui.add_space(4.0);
            key_box(ui, &q.fingerprint, TILE, 16.0);
            ui.label(muted(
                "This desk's key. It should match the key in that desk's Settings.",
            ));
            ui.add_space(6.0);
            ui.label(RichText::new("Let it connect?").font(semibold(15.0)));
            let mut answer = |verdict| out.push(Command::Answer { id: q.id, verdict });
            ui.horizontal(|ui| {
                let each = vec2(
                    (ui.available_width() - 2.0 * ui.spacing().item_spacing.x) / 3.0,
                    48.0,
                );
                if ui.add(button("No", CORAL, 48.0).min_size(each)).clicked() {
                    answer(Verdict::No);
                }
                if ui
                    .add(button("This time", WHITE, 48.0).min_size(each))
                    .clicked()
                {
                    answer(Verdict::Once);
                }
                if ui
                    .add(primary("Always").fill(GREEN).min_size(each))
                    .clicked()
                {
                    answer(Verdict::Always);
                }
            });
            if ui
                .link(RichText::new("Never for this desk").size(14.0))
                .clicked()
            {
                answer(Verdict::Never);
            }
        });
}

/// An example state, for pictures and tests of the window.
pub fn sample_state() -> AppState {
    let key = |seed: u8| -> String {
        (0..32u8)
            .map(|i| format!("{:02x}", i.wrapping_mul(37).wrapping_add(seed)))
            .collect()
    };
    let now = 1_795_003_200;
    let desk = |seed: u8, name: &str, os: &str, address: &str| Desk {
        fingerprint: key(seed),
        name: name.into(),
        os: os.into(),
        address: address.into(),
        connect_to: Some(format!("{address}:28326")),
        ..Desk::default()
    };
    AppState {
        version: env!("CARGO_PKG_VERSION").to_string(),
        device_name: "studio-desktop".into(),
        addresses: vec!["100.64.0.12".into()],
        fingerprint: key(7),
        incoming: Incoming::Listening,
        sessions: Vec::new(),
        desks: vec![
            Desk {
                own_name: "Laptop".into(),
                permission: Some(Permission::Always),
                favorite: true,
                last_out: now - 3_600,
                last_in: now - 7_200,
                ..desk(21, "Laptop", "Windows 11", "100.64.0.20")
            },
            Desk {
                last_out: now - 3 * 86_400,
                ..desk(42, "render-box", "Ubuntu 22.04", "100.64.0.31")
            },
            Desk {
                favorite: true,
                last_out: now - 8 * 86_400,
                permission: Some(Permission::Ask),
                last_in: now - 86_400,
                ..desk(63, "Living room", "macOS 15", "100.64.0.52")
            },
            Desk {
                connect_to: None,
                permission: Some(Permission::Never),
                tried_in: now - 4 * 86_400,
                ..desk(84, "OFFICE-PC-7", "Windows 10", "100.64.0.44")
            },
        ],
        online: HashMap::from([
            ("100.64.0.20:28326".to_string(), true),
            ("100.64.0.31:28326".to_string(), false),
        ]),
        dial: Dial::Idle,
        questions: VecDeque::new(),
        settings: Settings::default(),
        now,
        guests: Vec::new(),
        outgoing: 0,
        update: Updates::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A changelog entry reads as headings, bullets with their wrapped lines joined, and
    /// text, without Markdown's marks.
    #[test]
    fn release_notes_read_as_lines() {
        let notes = "Not released.\n\n### Added\n\n- **Updates**: the app finds a newer\n  release and installs it ([how](docs/x.md)).\n- `illum-sign`, for releases.\n\n### Fixed\n\n- One [thing.\n";
        assert_eq!(
            note_lines(notes),
            vec![
                NoteLine::Text("Not released.".into()),
                NoteLine::Heading("Added".into()),
                NoteLine::Bullet(
                    "**Updates**: the app finds a newer release and installs it (how).".into()
                ),
                NoteLine::Bullet("illum-sign, for releases.".into()),
                NoteLine::Heading("Fixed".into()),
                NoteLine::Bullet("One [thing.".into()),
            ]
        );
        assert!(note_lines("").is_empty());
    }

    /// A connected desk goes by the name you gave it, found by its address.
    #[test]
    fn a_connected_desk_goes_by_your_name_for_it() {
        let mut state = sample_state();
        state.sessions = vec!["LAPTOP-AVOEN7VP".into(), "render-box".into()];
        state.guests = vec![
            Guest {
                name: "LAPTOP-AVOEN7VP".into(),
                address: "100.64.0.20".into(),
                since: 0,
            },
            Guest {
                name: "render-box".into(),
                address: "100.64.0.31".into(),
                since: 0,
            },
        ];
        // The sample's desk at 100.64.0.20 is named "Laptop"; render-box has no name of yours.
        assert_eq!(connected_names(&state), ["Laptop", "render-box"]);
        state.guests.pop();
        assert_eq!(connected_names(&state), ["LAPTOP-AVOEN7VP", "render-box"]);
    }

    #[test]
    fn keys_read_in_four_rows() {
        let rows = key_rows(&"0123456789abcdef".repeat(4));
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0], "0123 4567 89AB CDEF");
        assert_eq!(key_start(&"ab".repeat(32)), "ABAB ABAB ABAB ABAB");
    }

    #[test]
    fn dates() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(1_793_000_000), "2026-10-26");
        assert_eq!(shown_address("100.64.0.20:28326"), "100.64.0.20");
        assert_eq!(shown_address("100.64.0.20:7171"), "100.64.0.20:7171");
        let now = 1_795_003_200;
        assert_eq!(day(now, now - 60), "today");
        assert_eq!(day(now, now - 86_400), "yesterday");
        assert_eq!(day(now, 1_793_000_000), "on 2026-10-26");
    }

    #[test]
    fn settings_file_tolerates_missing_and_unknown_fields() {
        // A file from 0.13, with a field that is gone and without the new ones.
        let s: Settings =
            serde_json::from_str(r#"{"max_fps":60,"allow_incoming":false,"pointer":"native"}"#)
                .unwrap();
        assert_eq!(s.max_fps, 60);
        assert!(s.start_on_boot && s.idle_minutes == 30);
        assert_eq!(s.new_desks, Permission::Ask);
        let text = serde_json::to_string(&Settings::default()).unwrap();
        assert_eq!(
            serde_json::from_str::<Settings>(&text).unwrap(),
            Settings::default()
        );
    }

    #[test]
    fn unsaved_changes_are_counted_per_setting() {
        let saved = Settings::default();
        assert!(unsaved(&saved, &saved).is_empty());
        let shown = Settings {
            max_fps: 60,
            latency_limit_ms: 80,
            let_sound: false,
            new_desks: Permission::Always,
            ..saved.clone()
        };
        assert_eq!(
            unsaved(&saved, &shown),
            [Item::Permission, Item::Actions, Item::Limits]
        );
        // Resetting Outgoing keeps what Incoming and General were set to.
        let reset = shown.with_outgoing_reset();
        assert_eq!(unsaved(&saved, &reset), [Item::Permission, Item::Actions]);
        // Every setting has a place in the menu.
        let listed: Vec<Item> = MENU
            .iter()
            .flat_map(|(_, items)| items.iter().copied())
            .collect();
        for item in unsaved(
            &saved,
            &Settings {
                start_on_boot: false,
                idle_minutes: 0,
                sound: false,
                clipboard: false,
                reconnect: false,
                allow_scale: true,
                exit_tab_transparency: 0,
                detailed_logs: true,
                ..shown
            },
        ) {
            assert!(listed.contains(&item), "{item:?} is missing from the menu");
        }
    }

    #[test]
    fn who_may_connect_is_locked_while_a_desk_is_connected() {
        let mut state = sample_state();
        state.guests.clear();
        state.sessions.clear();
        assert_eq!(lock_note(&state), None);
        state.sessions = vec!["LAPTOP".into()];
        let note = lock_note(&state).expect("locked");
        assert!(
            note.starts_with("Locked while 1 desk is connected."),
            "{note}"
        );
        state.sessions.push("TABLET".into());
        let note = lock_note(&state).expect("locked");
        assert!(
            note.starts_with("Locked while 2 desks are connected."),
            "{note}"
        );

        let saved = Settings::default();
        let shown = Settings {
            new_desks: Permission::Always,
            idle_minutes: 0,
            let_control: false,
            max_bitrate_mbps: 7.5,
            ..Settings::default()
        };
        let kept = shown.with_incoming_of(&saved);
        assert_eq!(
            kept.incoming(),
            saved.incoming(),
            "who may connect as it was"
        );
        assert_eq!(kept.max_bitrate_mbps, 7.5, "the rest as shown");
    }
}
