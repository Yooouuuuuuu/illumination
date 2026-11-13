// SPDX-License-Identifier: GPL-3.0-only
//! The app's home window: this desk, the remote desk to reach, the desks you know, settings,
//! and the two questions about keys (architecture §14).
//!
//! This module only draws and reports what the person did. [`AppState`] is what the rest of the
//! program knows (written by `illum::app`), [`Command`] is what the person asked for. The look
//! comes from the icon (`assets/icon`): a soft blue ground, navy outlines two pixels wide, the
//! strong yellow for the main action and the pale yellow for what is yours.

use crate::{KeyboardMode, PointerMode};
use egui::{
    Align, Color32, CornerRadius, FontFamily, FontId, Frame, Layout, Margin, RichText, Sense,
    Stroke, StrokeKind, Ui, Vec2, vec2,
};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;

pub mod color {
    use egui::Color32;
    pub const TILE: Color32 = Color32::from_rgb(0xDD, 0xE6, 0xF6);
    pub const NAVY: Color32 = Color32::from_rgb(0x1E, 0x2A, 0x4A);
    pub const YELLOW: Color32 = Color32::from_rgb(0xFF, 0xE0, 0x12);
    pub const SOFT: Color32 = Color32::from_rgb(0xFF, 0xF6, 0x8D);
    pub const WHITE: Color32 = Color32::WHITE;
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

/// What the person chose in Settings. Kept in `settings.json` in the home folder and applied to
/// the next connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub max_bitrate_mbps: f64,
    pub max_fps: u32,
    pub latency_limit_ms: u32,
    pub allow_scale: bool,
    pub reconnect: bool,
    pub keyboard: KeyboardMode,
    pub pointer: PointerMode,
    pub sound: bool,
    pub clipboard: bool,
    /// Listen for other desks while the app is open.
    pub allow_incoming: bool,
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
            pointer: PointerMode::Native,
            sound: true,
            clipboard: true,
            allow_incoming: true,
        }
    }
}

/// A desk this one has connected to (a pinned host key).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Desk {
    pub address: String,
    pub name: String,
    pub fingerprint: String,
    /// Unix seconds.
    pub first_seen: u64,
}

/// A desk that may connect to this one without asking (a paired device).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedDesk {
    pub fingerprint: String,
    pub name: String,
    pub last_address: String,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionKind {
    /// First connection to a desk: is the key it shows really its key?
    TrustHost,
    /// An unknown desk wants to connect to this one.
    AcceptDesk,
}

/// A question about a key, waiting for an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub id: u64,
    pub kind: QuestionKind,
    pub name: String,
    pub address: String,
    /// 64 hex digits.
    pub fingerprint: String,
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
    pub desks: Vec<Desk>,
    pub trusted: Vec<TrustedDesk>,
    pub dial: Dial,
    pub questions: VecDeque<Question>,
    pub settings: Settings,
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
        accept: bool,
    },
    ForgetDesk(String),
    ForgetTrusted(String),
    SaveSettings(Settings),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Page {
    #[default]
    Home,
    Settings,
}

/// What the window itself remembers between frames.
#[derive(Default)]
pub struct HomeUi {
    page: Page,
    address: String,
    view_only: bool,
    mute: bool,
    filter: String,
    /// The address just copied, to say so on the button for a moment.
    copied_at: Option<f64>,
}

impl HomeUi {
    /// Opens on Settings (for pictures of that page).
    pub fn on_settings() -> HomeUi {
        HomeUi {
            page: Page::Settings,
            ..HomeUi::default()
        }
    }

    /// Draws one frame and returns what the person asked for.
    pub fn draw(&mut self, ui: &mut Ui, state: &AppState) -> Vec<Command> {
        let mut out = Vec::new();
        self.top_bar(ui, state);
        egui::CentralPanel::default()
            .frame(Frame::new().fill(TILE))
            .show(ui, |ui| {
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
                                match self.page {
                                    Page::Home => self.home(ui, state, width, &mut out),
                                    Page::Settings => settings(ui, state, &mut out),
                                }
                            });
                        });
                        ui.add_space(32.0);
                    });
            });
        if let Some(q) = state.questions.front() {
            question(ui.ctx(), q, &mut out);
        }
        out
    }

    fn top_bar(&mut self, ui: &mut Ui, state: &AppState) {
        let bar = egui::Panel::top("bar")
            .frame(
                Frame::new()
                    .fill(WHITE)
                    .inner_margin(Margin::symmetric(24, 12)),
            )
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    match self.page {
                        Page::Home => {
                            logo(ui, 36.0);
                            if ui.available_width() > 430.0 {
                                ui.label(RichText::new("Illumination").font(heading(20.0)));
                            }
                        }
                        Page::Settings => {
                            if ui.add(button("Back", WHITE, 40.0)).clicked() {
                                self.page = Page::Home;
                            }
                            ui.label(RichText::new("Settings").font(heading(20.0)));
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if self.page == Page::Home
                            && ui.add(button("Settings", WHITE, 40.0)).clicked()
                        {
                            self.page = Page::Settings;
                        }
                        status_pill(ui, state);
                    });
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

    fn home(&mut self, ui: &mut Ui, state: &AppState, width: f32, out: &mut Vec<Command>) {
        // Side by side when there is room, one under the other on a narrow window.
        let gap = 24.0;
        if width >= 760.0 {
            let each = (width - gap) / 2.0;
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                ui.allocate_ui_with_layout(vec2(each, 0.0), Layout::top_down(Align::Min), |ui| {
                    this_desk(ui, state, each, 262.0, &mut self.copied_at, out)
                });
                ui.allocate_ui_with_layout(vec2(each, 0.0), Layout::top_down(Align::Min), |ui| {
                    self.remote_desk(ui, state, each, 262.0, out)
                });
            });
        } else {
            this_desk(ui, state, width, 0.0, &mut self.copied_at, out);
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
            ui.label(caps("Remote desk"));
            ui.label(RichText::new("IP address or hostname").font(semibold(14.0)));
            let dialing = matches!(state.dial, Dial::Dialing(_));
            let mut go = false;
            ui.horizontal(|ui| {
                let connect_width = 128.0;
                let field = ui.add_sized(
                    [ui.available_width() - connect_width - 10.0, 48.0],
                    text_field(&mut self.address, "100.64.0.20 or laptop", mono(17.0)),
                );
                go |= field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let label = if dialing { "Connecting…" } else { "Connect" };
                go |= ui
                    .add_enabled(!dialing, primary(label).min_size(vec2(connect_width, 48.0)))
                    .clicked();
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.view_only, "View only");
                ui.add_space(12.0);
                ui.checkbox(&mut self.mute, "Mute its sound");
            });
            if go && !dialing && !self.address.trim().is_empty() {
                out.push(Command::Connect {
                    target: self.address.trim().to_string(),
                    view_only: self.view_only,
                    mute: self.mute,
                });
            }
            match &state.dial {
                Dial::Idle => {
                    ui.label(muted(
                        "The first time, both desks show each other's key and ask you to \
                         confirm. After that you connect without prompts.",
                    ));
                }
                Dial::Dialing(target) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!("Reaching {target}…"));
                    });
                }
                Dial::Failed { target, message } => {
                    Frame::new()
                        .fill(SOFT)
                        .stroke(outline())
                        .corner_radius(CornerRadius::same(12))
                        .inner_margin(Margin::symmetric(14, 10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(
                                RichText::new(format!("Could not connect to {target}"))
                                    .font(semibold(14.0)),
                            );
                            ui.label(RichText::new(message).size(14.0));
                            if ui.add(button("OK", WHITE, 32.0)).clicked() {
                                out.push(Command::DismissDial);
                            }
                        });
                }
            }
        });
    }

    fn desks(&mut self, ui: &mut Ui, state: &AppState, width: f32, out: &mut Vec<Command>) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Your desks").font(heading(22.0)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_sized(
                    [240.0, 40.0],
                    text_field(&mut self.filter, "Find a desk", FontId::proportional(15.0)),
                );
            });
        });
        if state.desks.is_empty() {
            ui.label(muted(
                "No desks yet. Connect to one above and it is kept here.",
            ));
            return;
        }
        let needle = self.filter.trim().to_lowercase();
        let shown: Vec<&Desk> = state
            .desks
            .iter()
            .filter(|d| {
                needle.is_empty()
                    || d.name.to_lowercase().contains(&needle)
                    || d.address.to_lowercase().contains(&needle)
            })
            .collect();
        if shown.is_empty() {
            ui.label(muted("No desk matches."));
            return;
        }
        // As many columns as fit at 230 px or wider, all the same width.
        let gap = 20.0;
        let columns = (((width + gap) / (230.0 + gap)).floor() as usize).max(1);
        let each = (width - gap * (columns as f32 - 1.0)) / columns as f32;
        let dialing = matches!(state.dial, Dial::Dialing(_));
        for row in shown.chunks(columns) {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for desk in row {
                    ui.allocate_ui_with_layout(
                        vec2(each, 0.0),
                        Layout::top_down(Align::Min),
                        |ui| desk_card(ui, desk, each, dialing, self.view_only, self.mute, out),
                    );
                }
            });
            ui.add_space(8.0);
        }
    }
}

fn status_pill(ui: &mut Ui, state: &AppState) {
    let (text, fill) = match (&state.incoming, state.sessions.as_slice()) {
        (Incoming::Failed(_), _) => ("Not reachable".to_string(), WHITE),
        (_, [one]) => (format!("{one} is connected"), YELLOW),
        (_, [_, ..]) => (
            format!("{} desks are connected", state.sessions.len()),
            YELLOW,
        ),
        (Incoming::Listening, []) => ("Ready for connections".to_string(), SOFT),
        (Incoming::Off, []) => ("Not accepting connections".to_string(), WHITE),
    };
    Frame::new()
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
                // Filled when something can reach us, hollow when not: told apart without colour.
                if matches!(state.incoming, Incoming::Listening) || !state.sessions.is_empty() {
                    ui.painter().circle_filled(dot.center(), 5.0, NAVY);
                } else {
                    ui.painter()
                        .circle_stroke(dot.center(), 4.0, Stroke::new(2.0, NAVY));
                }
            });
        });
}

fn this_desk(
    ui: &mut Ui,
    state: &AppState,
    width: f32,
    min_height: f32,
    copied_at: &mut Option<f64>,
    out: &mut Vec<Command>,
) {
    card(ui, SOFT, |ui| {
        ui.set_width(width - 52.0);
        ui.set_min_height(min_height);
        ui.horizontal(|ui| {
            ui.label(caps("This desk"));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(&state.device_name).font(semibold(13.0)));
            });
        });
        match state.addresses.first() {
            Some(address) => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(address).font(mono(30.0)));
                    let now = ui.input(|i| i.time);
                    let just_copied = copied_at.is_some_and(|t| now - t < 1.5);
                    let label = if just_copied { "Copied" } else { "Copy" };
                    if ui.add(button(label, WHITE, 40.0)).clicked() {
                        ui.ctx().copy_text(address.clone());
                        *copied_at = Some(now);
                    }
                    if just_copied {
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(300));
                    }
                });
                if state.addresses.len() > 1 {
                    ui.label(
                        RichText::new(format!("also {}", state.addresses[1..].join(", ")))
                            .font(mono(13.0)),
                    );
                }
                ui.label(
                    RichText::new(
                        "Give this address to the desk that should reach you. It works on your own \
                     tailnet; nothing goes through a server.",
                    )
                    .size(14.0),
                );
            }
            None => {
                ui.label(RichText::new("No tailnet address").font(heading(22.0)));
                ui.label(RichText::new(
                    "Start Tailscale on this desk so others can reach it. You can still connect \
                     to other desks.",
                ).size(14.0));
            }
        }
        if let Incoming::Failed(why) = &state.incoming {
            ui.label(
                RichText::new(format!("Can't accept connections: {why}")).font(semibold(14.0)),
            );
        }
        let line = ui.available_rect_before_wrap();
        ui.painter().line_segment(
            [line.left_top(), egui::pos2(line.right(), line.top())],
            Stroke::new(OUTLINE, NAVY),
        );
        ui.add_space(8.0);
        let mut allow = state.settings.allow_incoming;
        if ui
            .checkbox(
                &mut allow,
                RichText::new("Allow incoming connections").font(semibold(15.0)),
            )
            .changed()
        {
            out.push(Command::SaveSettings(Settings {
                allow_incoming: allow,
                ..state.settings.clone()
            }));
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new("Key").size(13.0));
            ui.label(RichText::new(key_start(&state.fingerprint)).font(mono(13.0)))
                .on_hover_ui(|ui| {
                    for row in key_rows(&state.fingerprint) {
                        ui.label(RichText::new(row).font(mono(14.0)));
                    }
                });
            ui.label(RichText::new("(hover for all of it)").size(13.0));
        });
    });
}

fn desk_card(
    ui: &mut Ui,
    desk: &Desk,
    width: f32,
    dialing: bool,
    view_only: bool,
    mute: bool,
    out: &mut Vec<Command>,
) {
    Frame::new()
        .fill(WHITE)
        .stroke(outline())
        .corner_radius(CornerRadius::same(16))
        .show(ui, |ui| {
            ui.set_width(width - 4.0);
            ui.spacing_mut().item_spacing = vec2(10.0, 0.0);
            // A dark strip with a small screen in it, as the icon's "remote" screen.
            let (strip, _) = ui.allocate_exact_size(vec2(width - 4.0, 84.0), Sense::hover());
            let top = CornerRadius {
                nw: 14,
                ne: 14,
                sw: 0,
                se: 0,
            };
            ui.painter().rect_filled(strip, top, NAVY);
            let screen = egui::Rect::from_center_size(strip.center(), vec2(72.0, 48.0));
            ui.painter().rect(
                screen,
                CornerRadius::same(5),
                YELLOW,
                Stroke::new(2.5, TILE),
                StrokeKind::Inside,
            );
            Frame::new().inner_margin(Margin::same(16)).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.add(egui::Label::new(RichText::new(&desk.name).font(semibold(16.0))).truncate());
                ui.add(
                    egui::Label::new(
                        RichText::new(shown_address(&desk.address))
                            .font(mono(13.0))
                            .color(MUTED),
                    )
                    .truncate(),
                );
                ui.label(
                    RichText::new(format!("Known since {}", date(desk.first_seen)))
                        .size(13.0)
                        .color(MUTED),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let forget_width = 76.0;
                    let connect = ui.add_enabled(
                        !dialing,
                        button("Connect", WHITE, 40.0)
                            .min_size(vec2(ui.available_width() - forget_width - 10.0, 40.0)),
                    );
                    if connect.clicked() {
                        out.push(Command::Connect {
                            target: desk.address.clone(),
                            view_only,
                            mute,
                        });
                    }
                    if ui
                        .add(button("Forget", WHITE, 40.0).min_size(vec2(forget_width, 40.0)))
                        .on_hover_text("Forget this desk's key. The next connection asks again.")
                        .clicked()
                    {
                        out.push(Command::ForgetDesk(desk.address.clone()));
                    }
                });
            });
        });
}

fn settings(ui: &mut Ui, state: &AppState, out: &mut Vec<Command>) {
    let mut s = state.settings.clone();
    let title = |ui: &mut Ui, text: &str| {
        ui.label(RichText::new(text).font(heading(18.0)));
    };
    card(ui, WHITE, |ui| {
        ui.set_width(ui.available_width());
        title(ui, "Picture and speed");
        ui.label(muted(
            "The host starts at the top of these limits and steps down by itself when the \
             connection can't keep up.",
        ));
        egui::Grid::new("limits")
            .num_columns(2)
            .spacing(vec2(16.0, 10.0))
            .show(ui, |ui| {
                ui.label(RichText::new("Highest bitrate").font(semibold(14.0)));
                ui.add(
                    egui::DragValue::new(&mut s.max_bitrate_mbps)
                        .range(1.0..=200.0)
                        .speed(0.5)
                        .max_decimals(1)
                        .suffix(" Mb/s"),
                );
                ui.end_row();
                ui.label(RichText::new("Highest frame rate").font(semibold(14.0)));
                egui::ComboBox::from_id_salt("fps")
                    .selected_text(format!("{} fps", s.max_fps))
                    .show_ui(ui, |ui| {
                        for fps in [30, 60, 120] {
                            ui.selectable_value(&mut s.max_fps, fps, format!("{fps} fps"));
                        }
                    });
                ui.end_row();
                ui.label(RichText::new("Picture delay limit").font(semibold(14.0)));
                ui.add(
                    egui::DragValue::new(&mut s.latency_limit_ms)
                        .range(10..=500)
                        .suffix(" ms"),
                );
                ui.end_row();
            });
        ui.checkbox(
            &mut s.allow_scale,
            "Let the picture get smaller when the connection is very slow",
        );
        ui.checkbox(
            &mut s.reconnect,
            "Reconnect by itself when the connection drops",
        );
    });
    card(ui, WHITE, |ui| {
        ui.set_width(ui.available_width());
        title(ui, "Keyboard and pointer");
        let modes = [
            (
                KeyboardMode::Remote,
                "Remote",
                "Every key goes to the host, Win and Alt+Tab included. Its layout and input \
                 method apply.",
            ),
            (
                KeyboardMode::Local,
                "Local",
                "Type with this desk's input method, for a host that lacks your language.",
            ),
            (
                KeyboardMode::Auto,
                "Auto",
                "Remote when both desks use the same layout, otherwise Local.",
            ),
        ];
        for (mode, name, what) in modes {
            let chosen = s.keyboard == mode;
            let frame = Frame::new()
                .fill(if chosen { SOFT } else { WHITE })
                .stroke(Stroke::new(OUTLINE, if chosen { NAVY } else { LINE }))
                .corner_radius(CornerRadius::same(12))
                .inner_margin(Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.radio_value(
                        &mut s.keyboard,
                        mode,
                        RichText::new(name).font(semibold(15.0)),
                    );
                    ui.label(RichText::new(what).size(14.0));
                });
            if frame.response.interact(Sense::click()).clicked() {
                s.keyboard = mode;
            }
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new("Pointer").font(semibold(14.0)));
            let name = |p: PointerMode| match p {
                PointerMode::Native => "Native: my pointer, in the host's shape",
                PointerMode::Embedded => "Embedded: the host's cursor drawn in the picture",
            };
            egui::ComboBox::from_id_salt("pointer")
                .width(340.0)
                .selected_text(name(s.pointer))
                .show_ui(ui, |ui| {
                    for p in [PointerMode::Native, PointerMode::Embedded] {
                        ui.selectable_value(&mut s.pointer, p, name(p));
                    }
                });
        });
    });
    card(ui, WHITE, |ui| {
        ui.set_width(ui.available_width());
        title(ui, "Sound and clipboard");
        ui.checkbox(&mut s.sound, "Play the host's sound here");
        ui.checkbox(&mut s.clipboard, "Share the clipboard (text, both ways)");
    });
    card(ui, WHITE, |ui| {
        ui.set_width(ui.available_width());
        title(ui, "Trusted desks");
        ui.label(muted(
            "These connect to this desk without asking. Forget one and it has to be accepted \
             again.",
        ));
        if state.trusted.is_empty() {
            ui.label(muted("None yet."));
        }
        for desk in &state.trusted {
            Frame::new()
                .stroke(Stroke::new(OUTLINE, LINE))
                .corner_radius(CornerRadius::same(12))
                .inner_margin(Margin::symmetric(14, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&desk.name).font(semibold(15.0)));
                        ui.label(
                            RichText::new(key_start(&desk.fingerprint))
                                .font(mono(13.0))
                                .color(MUTED),
                        );
                        ui.label(muted(format!("last from {}", desk.last_address)));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.add(button("Forget", WHITE, 36.0)).clicked() {
                                out.push(Command::ForgetTrusted(desk.fingerprint.clone()));
                            }
                        });
                    });
                });
        }
    });
    ui.horizontal(|ui| {
        ui.label(muted(format!("Illumination {}", state.version)));
        ui.label(muted("GPL-3.0-only"));
        ui.hyperlink_to(
            RichText::new("Source code").size(14.0),
            "https://github.com/Yooouuuuuuu/illumination",
        );
    });
    ui.label(muted("Changes apply to the next connection."));
    if s != state.settings {
        out.push(Command::SaveSettings(s));
    }
}

/// The question in front of everything else: one of the two key checks.
fn question(ctx: &egui::Context, q: &Question, out: &mut Vec<Command>) {
    let frame = Frame::new()
        .fill(WHITE)
        .stroke(outline())
        .corner_radius(CornerRadius::same(16));
    egui::Modal::new(egui::Id::unique(("question", q.id)))
        .frame(frame)
        .backdrop_color(Color32::from_rgba_unmultiplied(0x1E, 0x2A, 0x4A, 150))
        .show(ctx, |ui| {
            ui.set_width(432.0);
            ui.spacing_mut().item_spacing.y = 0.0;
            let (band, title, small) = match q.kind {
                QuestionKind::AcceptDesk => (
                    YELLOW,
                    format!("{} wants to connect", q.name),
                    "Incoming connection",
                ),
                QuestionKind::TrustHost => {
                    (TILE, format!("Is this {}?", q.name), "First connection")
                }
            };
            let top = CornerRadius {
                nw: 14,
                ne: 14,
                sw: 0,
                se: 0,
            };
            let head = Frame::new()
                .fill(band)
                .corner_radius(top)
                .inner_margin(Margin::same(24))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.label(caps(small));
                    ui.label(RichText::new(title).font(heading(24.0)));
                    ui.label(RichText::new(format!("at {}", q.address)).font(mono(14.0)));
                });
            let line = head.response.rect;
            ui.painter().line_segment(
                [line.left_bottom(), line.right_bottom()],
                Stroke::new(OUTLINE, NAVY),
            );
            Frame::new().inner_margin(Margin::same(24)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 12.0;
                let (lead, check, yes, no) = match q.kind {
                    QuestionKind::AcceptDesk => (
                        "It shows this key:",
                        "It should match the key on that desk's own screen. Accepting lets it \
                         see this screen, use the keyboard and mouse, share the clipboard and \
                         hear this desk's sound, now and next time without asking.",
                        "Accept and trust",
                        "Decline",
                    ),
                    QuestionKind::TrustHost => (
                        "You have not connected to it before. It shows this key:",
                        "Compare it with the key on that desk's home screen, under This desk. \
                         If they match, nobody is in between. You are asked once; a different \
                         key later is refused.",
                        "They match, connect",
                        "Cancel",
                    ),
                };
                ui.label(lead);
                key_box(
                    ui,
                    &q.fingerprint,
                    if q.kind == QuestionKind::TrustHost {
                        SOFT
                    } else {
                        TILE
                    },
                    17.0,
                );
                ui.label(muted(check));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let w = ui.available_width();
                    if ui
                        .add(button(no, WHITE, 48.0).min_size(vec2(w * 0.34, 48.0)))
                        .clicked()
                    {
                        out.push(Command::Answer {
                            id: q.id,
                            accept: false,
                        });
                    }
                    if ui
                        .add(primary(yes).min_size(vec2(ui.available_width(), 48.0)))
                        .clicked()
                    {
                        out.push(Command::Answer {
                            id: q.id,
                            accept: true,
                        });
                    }
                });
            });
        });
}

/// An example state, for pictures and tests of the window.
pub fn sample_state() -> AppState {
    let key = |seed: u8| -> String {
        (0..32u8)
            .map(|i| format!("{:02x}", i.wrapping_mul(37).wrapping_add(seed)))
            .collect()
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
                address: "100.64.0.20:28326".into(),
                name: "studio-laptop".into(),
                fingerprint: key(21),
                first_seen: 1_793_000_000,
            },
            Desk {
                address: "100.64.0.31:28326".into(),
                name: "render-box".into(),
                fingerprint: key(42),
                first_seen: 1_793_500_000,
            },
            Desk {
                address: "living-room:28326".into(),
                name: "living-room".into(),
                fingerprint: key(63),
                first_seen: 1_794_000_000,
            },
        ],
        trusted: vec![TrustedDesk {
            fingerprint: key(21),
            name: "studio-laptop".into(),
            last_address: "100.64.0.20".into(),
        }],
        dial: Dial::Idle,
        questions: VecDeque::new(),
        settings: Settings::default(),
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
        assert_eq!(key_start("abcd"), "ABCD");
    }

    #[test]
    fn dates() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(951_782_400), "2000-02-29");
        assert_eq!(date(1_794_528_000), "2026-11-13");
    }

    #[test]
    fn settings_file_tolerates_missing_and_unknown_fields() {
        let s: Settings = serde_json::from_str(r#"{"max_fps":60,"later":true}"#).unwrap();
        assert_eq!(s.max_fps, 60);
        assert_eq!(s.keyboard, KeyboardMode::Remote);
        let text = serde_json::to_string(&Settings::default()).unwrap();
        assert!(text.contains(r#""keyboard":"remote""#), "{text}");
        assert_eq!(
            serde_json::from_str::<Settings>(&text).unwrap(),
            Settings::default()
        );
    }
}
