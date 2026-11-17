// SPDX-License-Identifier: GPL-3.0-only
//! The app's window: this desk, the desk to reach, the desks you know, settings, and the
//! question about a desk that wants in (architecture §14).
//!
//! This module only draws and reports what the person did. [`AppState`] is what the rest of the
//! program knows (written by `illum::app`), [`Command`] is what the person asked for. The look
//! comes from the icon (`assets/icon`): a soft blue ground, navy outlines, the strong yellow
//! for the main action and for what is active now. Three more colours each mean one thing
//! wherever they appear: green is open or allowed, blue is waiting for an answer, coral is
//! closed, refused or broken.

use crate::{KeyboardMode, PointerMode};
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
    /// Waiting for an answer: connecting, permission "ask".
    pub const BLUE: Color32 = Color32::from_rgb(0xC9, 0xDB, 0xFF);
    /// Closed, refused, broken: cannot be reached, permission "never", ending something.
    pub const CORAL: Color32 = Color32::from_rgb(0xFF, 0xB9, 0xA8);
    /// Secondary text: navy lightened, still 8:1 on white.
    pub const MUTED: Color32 = Color32::from_rgb(0x44, 0x50, 0x6E);
    /// Quiet outlines inside a card.
    pub const LINE: Color32 = Color32::from_rgb(0xC5, 0xD0, 0xE6);
}
use color::*;

const OUTLINE: f32 = 2.0;
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
    pub keyboard: KeyboardMode,
    pub pointer: PointerMode,
    pub sound: bool,
    pub clipboard: bool,
    // Incoming: when another desk connects here.
    /// What a connected desk may do, besides seeing the screen.
    pub let_control: bool,
    pub let_sound: bool,
    pub let_clipboard: bool,
    /// The permission of a desk that has none of its own: refused (true) or asked about.
    pub refuse_new: bool,
    /// A connected desk is disconnected after this long without input from it; 0: never.
    pub idle_minutes: u32,
    // General.
    /// Start with the system, in the tray, so this desk can be reached after a restart.
    pub start_on_boot: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            max_bitrate_mbps: 20.0,
            max_fps: 30,
            latency_limit_ms: 50,
            allow_scale: false,
            reconnect: true,
            keyboard: KeyboardMode::Remote,
            pointer: PointerMode::Desk,
            sound: true,
            clipboard: true,
            let_control: true,
            let_sound: true,
            let_clipboard: true,
            refuse_new: false,
            idle_minutes: 30,
            start_on_boot: true,
        }
    }
}

impl Settings {
    /// The outgoing settings put back to what a new install has; the rest is kept.
    pub fn with_outgoing_reset(&self) -> Settings {
        let fresh = Settings::default();
        Settings {
            let_control: self.let_control,
            let_sound: self.let_sound,
            let_clipboard: self.let_clipboard,
            refuse_new: self.refuse_new,
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

/// A small tag: a desk's own permission, online or offline.
fn tag(ui: &mut Ui, text: &str, fill: Color32, round: bool) {
    Frame::new()
        .fill(fill)
        .stroke(outline())
        .corner_radius(CornerRadius::same(if round { 255 } else { 8 }))
        .inner_margin(Margin::symmetric(10, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text.to_string()).font(semibold(12.0)));
        });
}

/// The words and the colour of a permission.
pub fn permission_look(permission: Permission) -> (&'static str, Color32) {
    match permission {
        Permission::Always => ("Always", GREEN),
        Permission::Ask => ("Ask", BLUE),
        Permission::Never => ("Never", CORAL),
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
    Permission,
    Actions,
    Idle,
    Limits,
    Keyboard,
    Pointer,
    Sound,
    Clipboard,
    Reconnect,
    Shrink,
    About,
}

const MENU: [(&str, &[(Item, &str)]); 4] = [
    (
        "General",
        &[
            (Item::StartOnBoot, "Start on boot"),
            (Item::Key, "This desk's key"),
        ],
    ),
    (
        "Incoming",
        &[
            (Item::Permission, "Default permission"),
            (Item::Actions, "Allowed actions"),
            (Item::Idle, "Disconnect when idle"),
        ],
    ),
    (
        "Outgoing",
        &[
            (Item::Limits, "Picture limits"),
            (Item::Keyboard, "Keyboard"),
            (Item::Pointer, "Pointer"),
            (Item::Sound, "Sound"),
            (Item::Clipboard, "Clipboard"),
            (Item::Reconnect, "Reconnect"),
            (Item::Shrink, "Shrink picture"),
        ],
    ),
    ("About", &[(Item::About, "Version")]),
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
    differs(Item::Permission, saved.refuse_new != shown.refuse_new);
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
    differs(Item::Keyboard, saved.keyboard != shown.keyboard);
    differs(Item::Pointer, saved.pointer != shown.pointer);
    differs(Item::Sound, saved.sound != shown.sound);
    differs(Item::Clipboard, saved.clipboard != shown.clipboard);
    differs(Item::Reconnect, saved.reconnect != shown.reconnect);
    differs(Item::Shrink, saved.allow_scale != shown.allow_scale);
    items
}

/// A desk as the edit dialog holds it until Save.
struct Editing {
    desk: Desk,
    name: String,
    permission: Option<Permission>,
    favorite: bool,
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
    /// The setting the menu was asked to bring into view.
    jump: Option<Item>,
    /// Asked to leave Settings with unsaved changes.
    leaving: bool,
    /// The "connected here" list is open.
    connected: bool,
    /// Asked to close while something is connected.
    closing: bool,
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
            favorite: desk.favorite,
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
                Page::Settings => self.settings(ui, state),
            });
        self.edit_dialog(ui.ctx(), &mut out);
        self.leave_dialog(ui.ctx(), &mut out);
        self.connected_dialog(ui.ctx(), state, &mut out);
        self.close_dialog(ui.ctx(), state, &mut out);
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
                            ui.label(RichText::new("Illumination").font(heading(20.0)));
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.add(button("Settings", WHITE, 40.0)).clicked() {
                                self.page = Page::Settings;
                            }
                            if status_pill(ui, state) {
                                self.connected = true;
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
                            // Always in sight, wherever the page is scrolled to.
                            let save = ui.add_enabled(
                                changes > 0,
                                primary("Save").min_size(vec2(96.0, 40.0)),
                            );
                            if save.clicked()
                                && let Some(pending) = &self.pending
                            {
                                out.push(Command::SaveSettings(pending.clone()));
                            }
                            if ui
                                .add_enabled(changes > 0, button("Discard", WHITE, 40.0))
                                .clicked()
                            {
                                self.pending = Some(state.settings.clone());
                            }
                            if changes > 0 {
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
        self.leaving = false;
        self.jump = None;
    }

    fn home(&mut self, ui: &mut Ui, state: &AppState, width: f32, out: &mut Vec<Command>) {
        // Side by side when there is room, one under the other on a narrow window.
        let gap = 24.0;
        if width >= 760.0 {
            let each = (width - gap) / 2.0;
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                ui.allocate_ui_with_layout(vec2(each, 0.0), Layout::top_down(Align::Min), |ui| {
                    this_desk(ui, state, each, 150.0)
                });
                ui.allocate_ui_with_layout(vec2(each, 0.0), Layout::top_down(Align::Min), |ui| {
                    self.remote_desk(ui, state, each, 150.0, out)
                });
            });
        } else {
            this_desk(ui, state, width, 0.0);
            ui.add_space(12.0);
            self.remote_desk(ui, state, width, 0.0, out);
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
                (Tab::Favorites, _) => "No favorites yet. Star a desk and it is kept here.",
                (Tab::Recent, _) => "No desks yet. Connect to one above and it is kept here.",
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
                                Some(OnCard::Star) => out.push(Command::Favorite {
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

    fn edit_dialog(&mut self, ctx: &egui::Context, out: &mut Vec<Command>) {
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
            ui.label(RichText::new("Permission").font(semibold(14.0)));
            let each = vec2((ui.available_width() - 10.0) / 2.0, 44.0);
            let choices: [(Option<Permission>, &str, Color32); 4] = [
                (None, "Default", SOFT),
                (Some(Permission::Always), "Always", GREEN),
                (Some(Permission::Ask), "Ask", BLUE),
                (Some(Permission::Never), "Never", CORAL),
            ];
            for pair in choices.chunks(2) {
                ui.horizontal(|ui| {
                    for (value, label, fill) in pair {
                        if choice(ui, label, editing.permission == *value, *fill, each) {
                            editing.permission = *value;
                        }
                    }
                });
            }
            ui.checkbox(&mut editing.favorite, "Favorite");
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.add(button("Forget", CORAL, 44.0)).clicked() {
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
                            favorite: editing.favorite,
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
                                    ui.label(RichText::new(&guest.name).font(heading(16.0)));
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

    fn settings(&mut self, ui: &mut Ui, state: &AppState) {
        let Some(mut s) = self.pending.clone() else {
            return;
        };
        let changed = unsaved(&state.settings, &s);
        let full = ui.available_width();
        let width = (full - 64.0).clamp(280.0, 1056.0);
        let side = ((full - width) / 2.0).max(0.0);
        let menu_width = if width >= 700.0 { 230.0 } else { 0.0 };
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
                        self.sections(ui, state, &mut s);
                        ui.add_space(24.0);
                    });
            });
        });
        self.pending = Some(s);
    }

    /// Every setting, grouped; a click brings it into view, a dot marks an unsaved change.
    fn menu(&mut self, ui: &mut Ui, changed: &[Item]) {
        Frame::new()
            .fill(WHITE)
            .stroke(outline())
            .corner_radius(CornerRadius::same(16))
            .inner_margin(Margin::same(8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 0.0;
                for (section, items) in MENU {
                    let head = egui::Button::new(RichText::new(section).font(heading(15.0)))
                        .fill(WHITE)
                        .stroke(Stroke::NONE)
                        .min_size(vec2(ui.available_width(), 34.0));
                    if ui.add(head).clicked() {
                        self.jump = Some(items[0].0);
                    }
                    for (item, label) in items {
                        if *item == Item::About {
                            continue;
                        }
                        let text = if changed.contains(item) {
                            format!("{label}  •")
                        } else {
                            (*label).to_string()
                        };
                        let entry = egui::Button::new(RichText::new(text).size(14.0))
                            .fill(WHITE)
                            .stroke(Stroke::NONE)
                            .min_size(vec2(ui.available_width(), 28.0));
                        if ui.add(entry).clicked() {
                            self.jump = Some(*item);
                        }
                    }
                    ui.add_space(6.0);
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

    fn sections(&mut self, ui: &mut Ui, state: &AppState, s: &mut Settings) {
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
            ui.label(muted("Other desks show it on first connection."));
        });
        card(ui, WHITE, |ui| {
            ui.set_width(ui.available_width());
            self.here(ui, Item::Permission);
            title(ui, "Incoming");
            ui.label(muted("When another desk connects here."));
            label(ui, "Default permission");
            ui.horizontal(|ui| {
                let each = vec2(150.0, 44.0);
                if choice(ui, "Ask", !s.refuse_new, BLUE, each) {
                    s.refuse_new = false;
                }
                if choice(ui, "Never", s.refuse_new, CORAL, each) {
                    s.refuse_new = true;
                }
            });
            ui.label(muted("A desk's own permission overrides this."));
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
        card(ui, WHITE, |ui| {
            ui.set_width(ui.available_width());
            self.here(ui, Item::Limits);
            ui.horizontal(|ui| {
                title(ui, "Outgoing");
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
                    label(ui, "Max bitrate");
                    ui.add(
                        egui::DragValue::new(&mut s.max_bitrate_mbps)
                            .range(1.0..=200.0)
                            .speed(0.5)
                            .max_decimals(1)
                            .suffix(" Mb/s"),
                    );
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
                            .suffix(" ms"),
                    );
                    ui.end_row();
                    self.here(ui, Item::Keyboard);
                    label(ui, "Keyboard");
                    let name = |mode: KeyboardMode| match mode {
                        KeyboardMode::Remote => "Remote",
                        KeyboardMode::Local => "Local",
                        KeyboardMode::Auto => "Auto",
                    };
                    egui::ComboBox::from_id_salt("keyboard")
                        .selected_text(name(s.keyboard))
                        .show_ui(ui, |ui| {
                            for mode in [
                                KeyboardMode::Remote,
                                KeyboardMode::Local,
                                KeyboardMode::Auto,
                            ] {
                                ui.selectable_value(&mut s.keyboard, mode, name(mode));
                            }
                        });
                    ui.end_row();
                    self.here(ui, Item::Pointer);
                    label(ui, "Pointer");
                    let name = |p: PointerMode| match p {
                        PointerMode::Desk => "This desk's",
                        PointerMode::Host => "The host's",
                    };
                    egui::ComboBox::from_id_salt("pointer")
                        .selected_text(name(s.pointer))
                        .show_ui(ui, |ui| {
                            for p in [PointerMode::Desk, PointerMode::Host] {
                                ui.selectable_value(&mut s.pointer, p, name(p));
                            }
                        });
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

/// This desk's state, top right. Returns whether it was clicked while a desk is connected
/// (which opens the list of them).
fn status_pill(ui: &mut Ui, state: &AppState) -> bool {
    let (text, fill, solid) = match (&state.incoming, state.sessions.as_slice()) {
        (Incoming::Failed(_), _) => ("Cannot be reached".to_string(), CORAL, false),
        (_, [one]) => (format!("{one} is connected"), YELLOW, true),
        (_, [_, ..]) => (
            format!("{} desks connected", state.sessions.len()),
            YELLOW,
            true,
        ),
        (Incoming::Listening, []) if state.settings.refuse_new => {
            ("New desks are refused".to_string(), WHITE, false)
        }
        (Incoming::Listening, []) => ("Ready for connections".to_string(), GREEN, true),
        (Incoming::Off, []) => ("Not listening".to_string(), WHITE, false),
    };
    let pill = Frame::new()
        .fill(fill)
        .stroke(outline())
        .corner_radius(CornerRadius::same(255))
        .inner_margin(Margin::symmetric(12, 6))
        .show(ui, |ui| {
            // The bar lays out from the right, so the text goes in first and the dot lands left of it.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.label(RichText::new(text).font(semibold(13.0)));
                let (dot, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                // Filled when open, hollow when not: told apart without colour.
                if solid {
                    ui.painter().circle_filled(dot.center(), 5.0, NAVY);
                } else {
                    ui.painter()
                        .circle_stroke(dot.center(), 4.0, Stroke::new(2.0, NAVY));
                }
            });
        });
    !state.sessions.is_empty() && pill.response.interact(Sense::click()).clicked()
}

fn this_desk(ui: &mut Ui, state: &AppState, width: f32, min_height: f32) {
    card(ui, SOFT, |ui| {
        ui.set_width(width - 52.0);
        ui.set_min_height(min_height);
        ui.label(caps("This desk"));
        match state.addresses.first() {
            Some(address) => {
                ui.label(RichText::new(address).font(mono(32.0)));
                if state.addresses.len() > 1 {
                    ui.label(
                        RichText::new(format!("also {}", state.addresses[1..].join(", ")))
                            .font(mono(13.0)),
                    );
                }
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
    });
}

enum OnCard {
    Connect,
    Edit,
    Star,
}

fn desk_card(
    ui: &mut Ui,
    desk: &Desk,
    state: &AppState,
    tab: Tab,
    width: f32,
    dialing: bool,
) -> Option<OnCard> {
    let mut did = None;
    Frame::new()
        .fill(WHITE)
        .stroke(outline())
        .corner_radius(CornerRadius::same(16))
        .inner_margin(Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.set_width(width - 36.0);
            ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
            ui.label(
                RichText::new(if desk.os.is_empty() { " " } else { &desk.os }.to_uppercase())
                    .font(heading(12.0))
                    .extra_letter_spacing(1.0)
                    .color(MUTED),
            );
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if star(ui, desk.favorite) {
                        did = Some(OnCard::Star);
                    }
                    if let Some(target) = &desk.connect_to {
                        match state.online.get(target) {
                            Some(true) => tag(ui, "Online", GREEN, true),
                            Some(false) => tag(ui, "Offline", WHITE, true),
                            None => {}
                        }
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(&desk.name).font(heading(18.0)))
                                .truncate(),
                        );
                    });
                });
            });
            ui.label(RichText::new(&desk.address).font(mono(13.0)).color(MUTED));
            let last = match tab {
                Tab::CameIn if desk.last_in >= desk.tried_in && desk.last_in > 0 => {
                    format!("Connected here {}", day(state.now, desk.last_in))
                }
                Tab::CameIn => format!("Tried to connect {}", day(state.now, desk.tried_in)),
                _ if desk.last_out > 0 => format!("Connected {}", day(state.now, desk.last_out)),
                _ if desk.last_in > 0 => {
                    format!("Connected here {}", day(state.now, desk.last_in))
                }
                _ => "Not connected yet".to_string(),
            };
            ui.label(RichText::new(last).size(13.0).color(MUTED));
            // Only a permission of its own is shown; without one it follows Settings.
            if let Some(permission) = desk.permission {
                let (word, fill) = permission_look(permission);
                tag(
                    ui,
                    &format!("Permission: {}", word.to_lowercase()),
                    fill,
                    false,
                );
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let can_connect = desk.connect_to.is_some();
                let w = ui.available_width();
                if can_connect
                    && ui
                        .add_enabled(
                            !dialing,
                            button("Connect", WHITE, 40.0).min_size(vec2(w * 0.58, 40.0)),
                        )
                        .clicked()
                {
                    did = Some(OnCard::Connect);
                }
                if ui
                    .add(button("Edit", WHITE, 40.0).min_size(vec2(ui.available_width(), 40.0)))
                    .clicked()
                {
                    did = Some(OnCard::Edit);
                }
            });
        });
    did
}

/// The star of a favourite: filled when it is one. Returns whether it was clicked.
fn star(ui: &mut Ui, on: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::click());
    let (c, outer, inner) = (rect.center(), 10.0f32, 4.2f32);
    let points: Vec<egui::Pos2> = (0..10)
        .map(|i| {
            let r = if i % 2 == 0 { outer } else { inner };
            let a = std::f32::consts::PI * (i as f32 / 5.0) - std::f32::consts::FRAC_PI_2;
            c + vec2(r * a.cos(), r * a.sin())
        })
        .collect();
    // A star is not convex: filled as a fan of triangles from its middle.
    if on {
        for i in 0..10 {
            ui.painter().add(egui::Shape::convex_polygon(
                vec![c, points[i], points[(i + 1) % 10]],
                YELLOW,
                Stroke::NONE,
            ));
        }
    }
    ui.painter()
        .add(egui::Shape::closed_line(points, Stroke::new(1.8, NAVY)));
    response
        .on_hover_text(if on {
            "Remove from favorites"
        } else {
            "Add to favorites"
        })
        .clicked()
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
                "Its key. Compare with the key in that desk's Settings.",
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(s.pointer, PointerMode::Desk);
        assert!(s.start_on_boot && s.idle_minutes == 30 && !s.refuse_new);
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
            refuse_new: true,
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
            .flat_map(|(_, items)| items.iter().map(|i| i.0))
            .collect();
        for item in unsaved(
            &saved,
            &Settings {
                start_on_boot: false,
                idle_minutes: 0,
                keyboard: KeyboardMode::Local,
                pointer: PointerMode::Host,
                sound: false,
                clipboard: false,
                reconnect: false,
                allow_scale: true,
                ..shown
            },
        ) {
            assert!(listed.contains(&item), "{item:?} is missing from the menu");
        }
    }
}
