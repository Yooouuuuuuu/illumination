// SPDX-License-Identifier: GPL-3.0-only
//! The session window's two bars (architecture §14): on top, who you are connected to and the
//! things you change while connected; at the bottom, how the connection is doing. The picture
//! sits between them. Mouse only: every key belongs to the host.

use crate::app::color::{MUTED, NAVY, SOFT, TILE, WHITE, YELLOW};
use crate::{KeyboardMode, PointerMode};
use egui::{
    Align, CornerRadius, FontFamily, FontId, Frame, Layout, Margin, RichText, Stroke, Ui, vec2,
};
use illum_media::scale::ViewMode;

/// What the bars show.
#[derive(Debug, Clone, PartialEq)]
pub struct BarState {
    pub host_name: String,
    pub host_address: String,
    /// False for view-only: no keyboard or pointer choices.
    pub controlling: bool,
    pub view: ViewMode,
    pub keyboard: KeyboardMode,
    /// Every key goes to the host (the keyboard is grabbed).
    pub keyboard_grabbed: bool,
    pub pointer: PointerMode,
    pub fullscreen: bool,
    /// Codec, quality and the numbers of the last second.
    pub status: String,
    /// Something to say instead (reconnecting, the picture is paused).
    pub notice: Option<String>,
}

/// What the person asked for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BarAction {
    View(ViewMode),
    Keyboard(KeyboardMode),
    Pointer(PointerMode),
    Fullscreen(bool),
    Disconnect,
}

/// The heights the bars took, in points: the picture goes between them.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BarSpace {
    pub top: f32,
    pub bottom: f32,
}

fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}

fn outline() -> Stroke {
    Stroke::new(2.0, NAVY)
}

/// Two or three choices side by side, the chosen one filled.
fn segments<T: Copy + PartialEq>(ui: &mut Ui, chosen: T, choices: &[(T, &str)]) -> Option<T> {
    let mut picked = None;
    Frame::new()
        .stroke(outline())
        .corner_radius(CornerRadius::same(10))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.horizontal(|ui| {
                let last = choices.len() - 1;
                for (i, (value, label)) in choices.iter().enumerate() {
                    let on = *value == chosen;
                    let radius = CornerRadius {
                        nw: if i == 0 { 8 } else { 0 },
                        sw: if i == 0 { 8 } else { 0 },
                        ne: if i == last { 8 } else { 0 },
                        se: if i == last { 8 } else { 0 },
                    };
                    let button =
                        egui::Button::new(RichText::new(*label).font(semibold(13.0)).color(NAVY))
                            .fill(if on { YELLOW } else { WHITE })
                            .stroke(Stroke::NONE)
                            .corner_radius(radius)
                            .min_size(vec2(0.0, 32.0));
                    if ui.add(button).clicked() && !on {
                        picked = Some(*value);
                    }
                }
            });
        });
    picked
}

fn plain(text: &str, fill: egui::Color32, text_color: egui::Color32) -> egui::Button<'static> {
    egui::Button::new(
        RichText::new(text.to_string())
            .font(semibold(13.0))
            .color(text_color),
    )
    .fill(fill)
    .stroke(outline())
    .corner_radius(CornerRadius::same(10))
    .min_size(vec2(0.0, 36.0))
}

/// Draws both bars and returns what was asked for and the room they took.
pub fn draw(ui: &mut Ui, state: &BarState) -> (Vec<BarAction>, BarSpace) {
    let mut out = Vec::new();
    let top = egui::Panel::top("session-top")
        .frame(
            Frame::new()
                .fill(WHITE)
                .inner_margin(Margin::symmetric(12, 8)),
        )
        .show_separator_line(false)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            ui.horizontal_wrapped(|ui| {
                Frame::new()
                    .fill(SOFT)
                    .stroke(outline())
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::symmetric(12, 7))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&state.host_name).font(semibold(14.0)));
                            ui.label(
                                RichText::new(&state.host_address)
                                    .font(FontId::monospace(12.0))
                                    .color(MUTED),
                            );
                        });
                    });
                if let Some(view) = segments(
                    ui,
                    state.view,
                    &[(ViewMode::Fit, "Fit"), (ViewMode::Original, "Original")],
                ) {
                    out.push(BarAction::View(view));
                }
                if state.controlling {
                    ui.label(RichText::new("Keyboard").size(13.0).color(MUTED));
                    if let Some(mode) = segments(
                        ui,
                        state.keyboard,
                        &[
                            (KeyboardMode::Remote, "Remote"),
                            (KeyboardMode::Local, "Local"),
                            (KeyboardMode::Auto, "Auto"),
                        ],
                    ) {
                        out.push(BarAction::Keyboard(mode));
                    }
                    ui.label(RichText::new("Pointer").size(13.0).color(MUTED));
                    if let Some(mode) = segments(
                        ui,
                        state.pointer,
                        &[
                            (PointerMode::Native, "Native"),
                            (PointerMode::Embedded, "Embedded"),
                        ],
                    ) {
                        out.push(BarAction::Pointer(mode));
                    }
                } else {
                    ui.label(RichText::new("View only").font(semibold(13.0)));
                }
                let label = if state.fullscreen {
                    "Leave full screen"
                } else {
                    "Full screen"
                };
                if ui.add(plain(label, WHITE, NAVY)).clicked() {
                    out.push(BarAction::Fullscreen(!state.fullscreen));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(plain("Disconnect", NAVY, WHITE)).clicked() {
                        out.push(BarAction::Disconnect);
                    }
                });
            });
        });
    let rect = top.response.rect;
    ui.painter().line_segment(
        [
            egui::pos2(rect.left(), rect.bottom() - 1.0),
            egui::pos2(rect.right(), rect.bottom() - 1.0),
        ],
        outline(),
    );
    let bottom = egui::Panel::bottom("session-bottom")
        .frame(
            Frame::new()
                .fill(TILE)
                .inner_margin(Margin::symmetric(12, 6)),
        )
        .show_separator_line(false)
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                match &state.notice {
                    Some(notice) => {
                        ui.label(RichText::new(notice).font(semibold(13.0)));
                    }
                    None => {
                        ui.label(RichText::new(&state.status).font(FontId::monospace(12.0)));
                    }
                }
                if state.controlling {
                    let keys = if state.keyboard_grabbed {
                        "Every key goes to the host. Click another window to get your keyboard back."
                    } else {
                        "Typing uses this desk's input method; Win and Alt+Tab go to the host."
                    };
                    ui.label(RichText::new(keys).size(12.0).color(MUTED));
                }
            });
        });
    let rect = bottom.response.rect;
    ui.painter().line_segment(
        [
            egui::pos2(rect.left(), rect.top() + 1.0),
            egui::pos2(rect.right(), rect.top() + 1.0),
        ],
        outline(),
    );
    (
        out,
        BarSpace {
            top: top.response.rect.height(),
            bottom: bottom.response.rect.height(),
        },
    )
}

/// An example, for pictures and tests.
pub fn sample() -> BarState {
    BarState {
        host_name: "studio-laptop".into(),
        host_address: "100.64.0.20".into(),
        controlling: true,
        view: ViewMode::Fit,
        keyboard: KeyboardMode::Remote,
        keyboard_grabbed: true,
        pointer: PointerMode::Native,
        fullscreen: false,
        status: "AV1 · rung 1/12: 20.0 Mb/s 30 fps, parity 5 % · 30 fps · 18 ms · input 14 ms"
            .into(),
        notice: None,
    }
}
