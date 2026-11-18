// SPDX-License-Identifier: GPL-3.0-only
//! The session window's two bars (architecture §14): a thin one on top with what is changed
//! often (which display, the picture's size, the window's mode) and a "More" menu for the
//! rest; a quiet line at the bottom saying who the session is with and how it is doing. The
//! picture sits between them. In Focus mode and in full screen both are hidden and a small
//! "Exit" tab at the top edge brings them back. Mouse only: every key belongs to the host.

use crate::app::color::{CORAL, CREAM, LINE, MUTED, NAVY, SOFT, WHITE, YELLOW};
use crate::{KeyboardMode, PointerMode};
use egui::{
    Align, Color32, CornerRadius, FontFamily, FontId, Frame, Layout, Margin, Rect, RichText, Sense,
    Stroke, Ui, pos2, vec2,
};
use illum_media::scale::ViewMode;

/// How much of the window the picture gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowMode {
    /// Both bars.
    #[default]
    Detail,
    /// The picture alone, in the window.
    Focus,
    /// The picture alone, over the whole screen.
    FullScreen,
}

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
    pub mode: WindowMode,
    /// With the bars hidden: the mouse is at the top edge, so the "Exit" tab shows.
    pub reveal: bool,
    /// Codec, quality and the numbers of the last second.
    pub status: String,
    /// Something to say instead (reconnecting, the picture is paused).
    pub notice: Option<String>,
    /// The host's displays, each in a few words; a choice is offered when there are several.
    pub displays: Vec<String>,
    /// The one being shown.
    pub display: u32,
    /// The host can show its Ctrl+Alt+Del screen when asked.
    pub can_sas: bool,
    /// The host can lock itself when asked.
    pub can_lock: bool,
    /// This window is a popped-out display: it has no bars, and its tab closes it.
    pub popped_out: bool,
    /// Several displays can be shown at once, and are; and the tiles are being arranged.
    pub can_all: bool,
    pub all: bool,
    pub arrange: bool,
    /// While arranging: the tiles, to mark each with its display's number.
    pub tiles: Vec<TileMark>,
}

/// A tile as the bars mark it while the tiles are arranged.
#[derive(Debug, Clone, PartialEq)]
pub struct TileMark {
    pub display: u32,
    /// Left, top, width and height in the window's pixels.
    pub rect: [f32; 4],
    /// It is the one being moved.
    pub held: bool,
}

/// What the person asked for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BarAction {
    View(ViewMode),
    Keyboard(KeyboardMode),
    Pointer(PointerMode),
    Mode(WindowMode),
    /// Show another of the host's displays.
    Display(u32),
    /// Ctrl+Alt+Del on the host.
    SecureAttention,
    /// Lock the host.
    Lock,
    /// Open another of the host's displays in a window of its own.
    PopOut(u32),
    /// Show the host's displays together, as tiles.
    All,
    /// Arrange the tiles (true), or go back to controlling the host.
    Arrange(bool),
    Disconnect,
}

/// What the bars remember between frames.
#[derive(Debug, Default)]
pub struct BarUi {
    /// The "More" menu is open.
    pub more: bool,
    /// How far the person slid the "Exit" tab from the middle of the top edge, in points.
    tab_offset: f32,
}

/// The room the bars took, in points: the picture goes between `top` and `bottom`, and the
/// mouse is the bars' inside `claims` too (the open menu, the "Exit" tab), not the host's.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BarSpace {
    pub top: f32,
    pub bottom: f32,
    pub claims: Vec<Rect>,
}

/// While the tiles are arranged: each with a frame and its display's number, so it is
/// plain what is moved and that the host is not being touched.
fn tile_marks(ui: &mut Ui, state: &BarState) {
    if !state.arrange {
        return;
    }
    let scale = ui.ctx().pixels_per_point();
    let painter = ui.ctx().layer_painter(egui::LayerId::background());
    for tile in &state.tiles {
        let [x, y, w, h] = tile.rect.map(|v| v / scale);
        let rect = Rect::from_min_size(pos2(x, y), vec2(w, h));
        painter.rect_stroke(
            rect.shrink(1.5),
            CornerRadius::ZERO,
            Stroke::new(3.0, if tile.held { YELLOW } else { SOFT }),
            egui::StrokeKind::Inside,
        );
        let badge = Rect::from_center_size(rect.center(), vec2(64.0, 64.0));
        painter.rect_filled(
            badge,
            CornerRadius::same(14),
            if tile.held { YELLOW } else { CREAM },
        );
        painter.rect_stroke(
            badge,
            CornerRadius::same(14),
            Stroke::new(1.0, EDGE),
            egui::StrokeKind::Inside,
        );
        painter.text(
            badge.center(),
            egui::Align2::CENTER_CENTER,
            (tile.display + 1).to_string(),
            semibold(34.0),
            NAVY,
        );
    }
}

fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}

/// The soft line between a bar and the picture.
const EDGE: Color32 = Color32::from_rgb(0xE6, 0xDC, 0x8A);

fn thin() -> Stroke {
    Stroke::new(1.5, NAVY)
}

/// A row of choices of which one is on: `[ 1 | 2 | 3 ]`.
fn segments<T: Copy + PartialEq>(ui: &mut Ui, chosen: T, choices: &[(T, &str)]) -> Option<T> {
    // In a row laid out from the right the first one drawn is the rightmost: drawn in
    // reverse there, so the choices read in the order given either way.
    let from_right = ui.layout().main_dir() == egui::Direction::RightToLeft;
    let mut order: Vec<&(T, &str)> = choices.iter().collect();
    if from_right {
        order.reverse();
    }
    let mut picked = None;
    Frame::new()
        .stroke(thin())
        .corner_radius(CornerRadius::same(8))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.horizontal(|ui| {
                let last = order.len() - 1;
                for (i, (value, label)) in order.into_iter().enumerate() {
                    let on = *value == chosen;
                    // Which end of the row this one is at, as seen.
                    let (left, right) = if from_right {
                        (i == last, i == 0)
                    } else {
                        (i == 0, i == last)
                    };
                    let radius = CornerRadius {
                        nw: if left { 7 } else { 0 },
                        sw: if left { 7 } else { 0 },
                        ne: if right { 7 } else { 0 },
                        se: if right { 7 } else { 0 },
                    };
                    let button =
                        egui::Button::new(RichText::new(*label).font(semibold(13.0)).color(NAVY))
                            .fill(if on { SOFT } else { WHITE })
                            .stroke(Stroke::NONE)
                            .corner_radius(radius)
                            .min_size(vec2(30.0, 24.0));
                    if ui.add(button).clicked() && !on {
                        picked = Some(*value);
                    }
                }
            });
        });
    picked
}

fn plain(text: &str, fill: Color32) -> egui::Button<'static> {
    egui::Button::new(
        RichText::new(text.to_string())
            .font(semibold(13.0))
            .color(NAVY),
    )
    .fill(fill)
    .stroke(thin())
    .corner_radius(CornerRadius::same(8))
    .min_size(vec2(0.0, 27.0))
}

/// One entry of the "More" menu; `on` marks the chosen one of a group.
fn entry(ui: &mut Ui, text: &str, on: bool) -> bool {
    let font = if on {
        semibold(14.0)
    } else {
        FontId::proportional(14.0)
    };
    let button = egui::Button::new(RichText::new(text.to_string()).font(font).color(NAVY))
        .fill(if on { SOFT } else { WHITE })
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(8))
        .min_size(vec2(ui.available_width(), 34.0));
    ui.add(button).clicked()
}

fn group(ui: &mut Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(
        RichText::new(text.to_uppercase())
            .font(semibold(11.0))
            .extra_letter_spacing(0.8)
            .color(MUTED),
    );
}

/// Draws the bars (or, with them hidden, the "Exit" tab) and returns what was asked for and
/// the room they took.
pub fn draw(ui: &mut Ui, state: &BarState, own: &mut BarUi) -> (Vec<BarAction>, BarSpace) {
    let mut out = Vec::new();
    let mut space = BarSpace::default();
    // Compact, as the bar is thin.
    ui.ctx().all_styles_mut(|style| {
        style.spacing.button_padding = vec2(10.0, 3.0);
        style.spacing.interact_size.y = 24.0;
    });
    if state.mode != WindowMode::Detail {
        own.more = false;
        exit_tab(ui, state, own, &mut out, &mut space);
        return (out, space);
    }
    let mut more_at = None;
    let top = egui::Panel::top("session-top")
        .frame(
            Frame::new()
                .fill(CREAM)
                .inner_margin(Margin::symmetric(12, 5)),
        )
        .show_separator_line(false)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            ui.horizontal_wrapped(|ui| {
                if state.displays.len() > 1 {
                    ui.label(RichText::new("Display").size(13.0).color(MUTED));
                    // `None` is "All": the displays together.
                    let mut numbers: Vec<(Option<u32>, String)> = (0..state.displays.len() as u32)
                        .map(|i| (Some(i), (i + 1).to_string()))
                        .collect();
                    if state.can_all {
                        numbers.push((None, "All".to_string()));
                    }
                    let choices: Vec<(Option<u32>, &str)> =
                        numbers.iter().map(|(i, n)| (*i, n.as_str())).collect();
                    let chosen = (!state.all).then_some(state.display);
                    match segments(ui, chosen, &choices) {
                        Some(Some(display)) => out.push(BarAction::Display(display)),
                        Some(None) => out.push(BarAction::All),
                        None => {}
                    }
                }
                if state.all {
                    // Together they are always fitted; what is chosen instead is whose the
                    // mouse is: the host's, or the tiles'.
                    if let Some(arrange) =
                        segments(ui, state.arrange, &[(false, "Control"), (true, "Arrange")])
                    {
                        out.push(BarAction::Arrange(arrange));
                    }
                } else if let Some(view) = segments(
                    ui,
                    state.view,
                    &[(ViewMode::Fit, "Fit"), (ViewMode::Original, "Original")],
                ) {
                    out.push(BarAction::View(view));
                }
                let more = ui.add(plain("More", if own.more { SOFT } else { WHITE }));
                if more.clicked() {
                    own.more = !own.more;
                }
                more_at = Some(more.rect);
                if !state.controlling {
                    ui.label(RichText::new("View only").font(semibold(13.0)));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Some(mode) = segments(
                        ui,
                        state.mode,
                        &[
                            (WindowMode::Detail, "Detail"),
                            (WindowMode::Focus, "Focus"),
                            (WindowMode::FullScreen, "Full screen"),
                        ],
                    ) {
                        out.push(BarAction::Mode(mode));
                    }
                });
            });
        });
    tile_marks(ui, state);
    let rect = top.response.rect;
    ui.painter().line_segment(
        [
            pos2(rect.left(), rect.bottom() - 0.5),
            pos2(rect.right(), rect.bottom() - 0.5),
        ],
        Stroke::new(1.0, EDGE),
    );
    space.top = rect.height();
    let bottom = egui::Panel::bottom("session-bottom")
        .frame(
            Frame::new()
                .fill(CREAM)
                .inner_margin(Margin::symmetric(12, 4)),
        )
        .show_separator_line(false)
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                ui.label(
                    RichText::new(format!(
                        "{} · {} · {}",
                        state.host_name, state.host_address, state.status
                    ))
                    .font(FontId::monospace(12.0))
                    .color(MUTED),
                );
                if let Some(notice) = &state.notice {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        Frame::new()
                            .fill(YELLOW)
                            .stroke(thin())
                            .corner_radius(CornerRadius::same(8))
                            .inner_margin(Margin::symmetric(10, 1))
                            .show(ui, |ui| {
                                ui.label(RichText::new(notice).font(semibold(12.0)));
                            });
                    });
                }
            });
        });
    let rect = bottom.response.rect;
    ui.painter().line_segment(
        [
            pos2(rect.left(), rect.top() + 0.5),
            pos2(rect.right(), rect.top() + 0.5),
        ],
        Stroke::new(1.0, EDGE),
    );
    space.bottom = rect.height();
    if let (true, Some(at)) = (own.more, more_at) {
        let menu = egui::Area::new(egui::Id::unique("session-more"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos2(at.left(), at.bottom() + 6.0))
            .show(ui.ctx(), |ui| {
                Frame::new()
                    .fill(WHITE)
                    .stroke(thin())
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_width(210.0);
                        ui.spacing_mut().item_spacing.y = 2.0;
                        more_menu(ui, state, &mut out);
                    });
            });
        space.claims.push(menu.response.rect);
        // A choice closes the menu.
        if !out.is_empty() {
            own.more = false;
        }
    }
    (out, space)
}

fn more_menu(ui: &mut Ui, state: &BarState, out: &mut Vec<BarAction>) {
    if state.controlling {
        group(ui, "Keyboard");
        for (mode, name) in [
            (KeyboardMode::Remote, "Remote"),
            (KeyboardMode::Local, "Local"),
            (KeyboardMode::Auto, "Auto"),
        ] {
            let on = state.keyboard == mode;
            if entry(ui, name, on) && !on {
                out.push(BarAction::Keyboard(mode));
            }
        }
        group(ui, "Pointer");
        for (mode, name) in [
            (PointerMode::Remote, "Remote"),
            (PointerMode::Local, "Local"),
        ] {
            let on = state.pointer == mode;
            if entry(ui, name, on) && !on {
                out.push(BarAction::Pointer(mode));
            }
        }
        if state.can_sas || state.can_lock {
            group(ui, "Host");
            if state.can_sas && entry(ui, "Send Ctrl+Alt+Del", false) {
                out.push(BarAction::SecureAttention);
            }
            if state.can_lock && entry(ui, "Lock", false) {
                out.push(BarAction::Lock);
            }
        }
        if state.displays.len() > 1 {
            group(ui, "Displays");
            for display in 0..state.displays.len() as u32 {
                if display != state.display
                    && entry(ui, &format!("Pop out display {}", display + 1), false)
                {
                    out.push(BarAction::PopOut(display));
                }
            }
        }
        ui.add_space(6.0);
        let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
        ui.painter().rect_filled(line, 0.0, LINE);
        ui.add_space(6.0);
    }
    let disconnect =
        egui::Button::new(RichText::new("Disconnect").font(semibold(14.0)).color(NAVY))
            .fill(CORAL)
            .stroke(thin())
            .corner_radius(CornerRadius::same(10))
            .min_size(vec2(ui.available_width(), 36.0));
    if ui.add(disconnect).clicked() {
        out.push(BarAction::Disconnect);
    }
}

/// With the bars hidden: a small tab at the top edge, shown while the mouse is up there. A
/// click brings the bars back; it can be slid sideways out of the way of what is under it.
/// It sits in the middle, not in a corner: the corners are where the host's own close button
/// and menus are.
fn exit_tab(
    ui: &mut Ui,
    state: &BarState,
    own: &mut BarUi,
    out: &mut Vec<BarAction>,
    space: &mut BarSpace,
) {
    let screen = ui.ctx().content_rect();
    // Something that needs the person (the host is about to disconnect, the link dropped)
    // is still said with the bars hidden: a small note over the picture, which lets the
    // mouse through.
    if let Some(notice) = &state.notice {
        egui::Area::new(egui::Id::unique("session-note"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_BOTTOM, vec2(0.0, -24.0))
            .interactable(false)
            .show(ui.ctx(), |ui| {
                Frame::new()
                    .fill(YELLOW)
                    .stroke(thin())
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::symmetric(12, 4))
                    .show(ui, |ui| {
                        ui.label(RichText::new(notice).font(semibold(13.0)));
                    });
            });
    }
    // The top edge always takes the mouse, so reaching it can be noticed at all.
    space
        .claims
        .push(Rect::from_min_size(screen.min, vec2(screen.width(), 2.0)));
    let size = vec2(52.0, 20.0);
    let reach = ((screen.width() - size.x) / 2.0).max(0.0);
    own.tab_offset = own.tab_offset.clamp(-reach, reach);
    let rect = Rect::from_min_size(
        pos2(
            screen.center().x - size.x / 2.0 + own.tab_offset,
            screen.top(),
        ),
        size,
    );
    // The mouse's while it is over where the tab is, shown or about to be.
    space.claims.push(rect);
    if !state.reveal {
        return;
    }
    egui::Area::new(egui::Id::unique("session-exit"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.min)
        .show(ui.ctx(), |ui| {
            let (tab, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
            ui.painter().rect(
                tab,
                CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: 8,
                    se: 8,
                },
                CREAM,
                Stroke::new(1.0, EDGE),
                egui::StrokeKind::Inside,
            );
            ui.painter().text(
                tab.center(),
                egui::Align2::CENTER_CENTER,
                if state.popped_out { "Close" } else { "Exit" },
                semibold(11.0),
                NAVY,
            );
            if response.dragged() {
                own.tab_offset += response.drag_delta().x;
            } else if response.clicked() {
                // A popped-out display has no bars to come back to: its tab closes it.
                out.push(if state.popped_out {
                    BarAction::Disconnect
                } else {
                    BarAction::Mode(WindowMode::Detail)
                });
            }
            response.on_hover_text(if state.popped_out {
                "Close this display's window"
            } else {
                "Exit focus mode"
            });
        });
}

/// An example, for pictures and tests.
pub fn sample() -> BarState {
    BarState {
        host_name: "Laptop".into(),
        host_address: "100.64.0.20".into(),
        controlling: true,
        view: ViewMode::Fit,
        keyboard: KeyboardMode::Remote,
        keyboard_grabbed: true,
        pointer: PointerMode::Remote,
        mode: WindowMode::Detail,
        reveal: false,
        status: "H.265 · 30 fps · 18 ms · input 12 ms".into(),
        notice: None,
        displays: vec!["2560 × 1440, main".into(), "1920 × 1080".into()],
        display: 0,
        can_sas: true,
        can_lock: true,
        popped_out: false,
        can_all: true,
        all: false,
        arrange: false,
        tiles: Vec::new(),
    }
}
