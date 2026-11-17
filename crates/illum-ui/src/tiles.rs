// SPDX-License-Identifier: GPL-3.0-only
//! Several of the host's displays in one window, as tiles (architecture §14).
//!
//! Two displays sit side by side, three or four in a two-by-two grid; more than four are not
//! shown together. Every tile starts the same size whatever the displays' own sizes are, and
//! the person may move the lines between them and swap two tiles. Each display is a stream
//! of its own, asked for at about the size its tile shows it at.

use serde::{Deserialize, Serialize};

/// How many displays are shown together at most.
pub const MAX_TILES: usize = 4;
/// The line between two tiles, in pixels.
const GAP: u32 = 2;
/// No tile is made narrower or lower than this share of the window.
const MIN_SHARE: f32 = 0.15;
/// The sizes a display is asked for, in percent of its own. Few, so that moving a line a
/// little does not start the stream anew.
const STEPS: [u32; 7] = [25, 33, 40, 50, 60, 75, 100];

/// A rectangle of the window, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Area {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Area {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= f64::from(self.x)
            && y >= f64::from(self.y)
            && x < f64::from(self.x + self.w)
            && y < f64::from(self.y + self.h)
    }
}

/// A line between tiles that can be moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// The upright line: moved left and right.
    Upright,
    /// The level line (three or four displays): moved up and down.
    Level,
    /// Where the two cross: moved both ways.
    Both,
}

/// Which display is in which place, and where the lines are. Kept per desk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    /// The displays, in the order of their places: left then right, upper row then lower.
    pub order: Vec<u32>,
    /// Where the upright line is, as a share of the width.
    pub fx: f32,
    /// Where the level line is, as a share of the height.
    pub fy: f32,
}

impl Layout {
    /// The arrangement for a host with `count` displays of which `current` is being shown:
    /// the one `saved` for it if that still fits the displays, else every tile the same.
    pub fn for_displays(count: usize, current: u32, saved: Option<&Layout>) -> Layout {
        let mut order: Vec<u32> = (0..count as u32).take(MAX_TILES).collect();
        if !order.contains(&current)
            && let Some(last) = order.last_mut()
        {
            *last = current;
        }
        if let Some(saved) = saved {
            let mut theirs = saved.order.clone();
            let mut ours = order.clone();
            theirs.sort_unstable();
            ours.sort_unstable();
            if theirs == ours && saved.fx.is_finite() && saved.fy.is_finite() {
                return Layout {
                    order: saved.order.clone(),
                    fx: saved.fx.clamp(MIN_SHARE, 1.0 - MIN_SHARE),
                    fy: saved.fy.clamp(MIN_SHARE, 1.0 - MIN_SHARE),
                };
            }
        }
        Layout {
            order,
            fx: 0.5,
            fy: 0.5,
        }
    }

    fn cuts(&self, area: Area) -> (u32, u32) {
        (
            area.x + (area.w as f32 * self.fx).round() as u32,
            area.y + (area.h as f32 * self.fy).round() as u32,
        )
    }

    /// Each display with the part of `area` it gets.
    pub fn tiles(&self, area: Area) -> Vec<(u32, Area)> {
        let n = self.order.len();
        if n <= 1 {
            return self.order.iter().map(|d| (*d, area)).collect();
        }
        let (cut_x, cut_y) = self.cuts(area);
        let half = GAP / 2;
        let columns = [
            (area.x, cut_x.saturating_sub(half + area.x).max(1)),
            (
                cut_x + half,
                (area.x + area.w).saturating_sub(cut_x + half).max(1),
            ),
        ];
        let rows = if n <= 2 {
            [(area.y, area.h.max(1)), (area.y, area.h.max(1))]
        } else {
            [
                (area.y, cut_y.saturating_sub(half + area.y).max(1)),
                (
                    cut_y + half,
                    (area.y + area.h).saturating_sub(cut_y + half).max(1),
                ),
            ]
        };
        self.order
            .iter()
            .enumerate()
            .map(|(i, display)| {
                let (x, w) = columns[i % 2];
                let (y, h) = rows[i / 2];
                (*display, Area { x, y, w, h })
            })
            .collect()
    }

    /// The place (not the display) at a point of the window.
    pub fn place_at(&self, area: Area, x: f64, y: f64) -> Option<usize> {
        self.tiles(area)
            .iter()
            .position(|(_, tile)| tile.contains(x, y))
    }

    /// The line within `grip` pixels of a point, if any.
    pub fn edge_at(&self, area: Area, x: f64, y: f64, grip: f64) -> Option<Edge> {
        let n = self.order.len();
        if n < 2 || !area.contains(x, y) {
            return None;
        }
        let (cut_x, cut_y) = self.cuts(area);
        let upright = (x - f64::from(cut_x)).abs() <= grip;
        let level = n > 2 && (y - f64::from(cut_y)).abs() <= grip;
        match (upright, level) {
            (true, true) => Some(Edge::Both),
            (true, false) => Some(Edge::Upright),
            (false, true) => Some(Edge::Level),
            (false, false) => None,
        }
    }

    /// Moves a line to a point of the window.
    pub fn move_edge(&mut self, area: Area, edge: Edge, x: f64, y: f64) {
        let share = |at: f64, start: u32, length: u32| {
            (((at - f64::from(start)) / f64::from(length.max(1))) as f32)
                .clamp(MIN_SHARE, 1.0 - MIN_SHARE)
        };
        if matches!(edge, Edge::Upright | Edge::Both) {
            self.fx = share(x, area.x, area.w);
        }
        if matches!(edge, Edge::Level | Edge::Both) {
            self.fy = share(y, area.y, area.h);
        }
    }

    /// Two places exchange their displays.
    pub fn swap(&mut self, a: usize, b: usize) {
        if a < self.order.len() && b < self.order.len() {
            self.order.swap(a, b);
        }
    }
}

/// The size to ask a display for, in percent of its own, so that it fills a tile without
/// being sent larger than it is shown: the smallest step that is not smaller than the tile.
pub fn scale_step(tile: Area, native: (u32, u32)) -> u32 {
    if native.0 == 0 || native.1 == 0 {
        return 100;
    }
    let wanted = (f64::from(tile.w) / f64::from(native.0))
        .min(f64::from(tile.h) / f64::from(native.1))
        * 100.0;
    STEPS
        .into_iter()
        .find(|step| f64::from(*step) >= wanted - 0.5)
        .unwrap_or(100)
}

/// The size in a display's few words, which begin with it: "2560 × 1440, main".
pub fn size_in(name: &str) -> Option<(u32, u32)> {
    let (w, rest) = name.split_once('×')?;
    let h: String = rest
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    Some((w.trim().parse().ok()?, h.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Area = Area {
        x: 0,
        y: 30,
        w: 1920,
        h: 1000,
    };

    #[test]
    fn two_displays_sit_side_by_side_the_same_size() {
        let layout = Layout::for_displays(2, 0, None);
        let tiles = layout.tiles(WINDOW);
        assert_eq!(tiles.len(), 2);
        assert_eq!((tiles[0].0, tiles[1].0), (0, 1));
        assert_eq!(tiles[0].1.w, tiles[1].1.w);
        assert_eq!((tiles[0].1.h, tiles[1].1.h), (1000, 1000));
        assert!(tiles[0].1.x + tiles[0].1.w < tiles[1].1.x, "a line between");
        assert_eq!(tiles[1].1.x + tiles[1].1.w, 1920);
    }

    #[test]
    fn three_and_four_are_a_grid_and_more_than_four_are_not_shown() {
        let three = Layout::for_displays(3, 0, None).tiles(WINDOW);
        assert_eq!(three.len(), 3);
        assert_eq!(three[2].1.x, three[0].1.x, "the third is under the first");
        assert!(three[2].1.y > three[0].1.y);
        let four = Layout::for_displays(4, 0, None).tiles(WINDOW);
        assert_eq!(four[3].1.x, four[1].1.x);
        assert_eq!(four[3].1.y, four[2].1.y);
        // Six displays: four are shown, and the one being looked at is among them.
        let six = Layout::for_displays(6, 5, None);
        assert_eq!(six.order, [0, 1, 2, 5]);
    }

    #[test]
    fn a_point_finds_its_place_and_the_lines() {
        let layout = Layout::for_displays(4, 0, None);
        assert_eq!(layout.place_at(WINDOW, 100.0, 100.0), Some(0));
        assert_eq!(layout.place_at(WINDOW, 1800.0, 100.0), Some(1));
        assert_eq!(layout.place_at(WINDOW, 100.0, 900.0), Some(2));
        assert_eq!(layout.place_at(WINDOW, 1800.0, 900.0), Some(3));
        assert_eq!(layout.place_at(WINDOW, 100.0, 10.0), None, "the bar");
        assert_eq!(
            layout.edge_at(WINDOW, 961.0, 100.0, 6.0),
            Some(Edge::Upright)
        );
        assert_eq!(layout.edge_at(WINDOW, 100.0, 532.0, 6.0), Some(Edge::Level));
        assert_eq!(layout.edge_at(WINDOW, 960.0, 530.0, 6.0), Some(Edge::Both));
        assert_eq!(layout.edge_at(WINDOW, 400.0, 300.0, 6.0), None);
        // Two displays have no level line.
        let two = Layout::for_displays(2, 0, None);
        assert_eq!(two.edge_at(WINDOW, 100.0, 530.0, 6.0), None);
    }

    #[test]
    fn a_line_moves_but_leaves_every_tile_some_room() {
        let mut layout = Layout::for_displays(2, 0, None);
        layout.move_edge(WINDOW, Edge::Upright, 1440.0, 0.0);
        assert!((layout.fx - 0.75).abs() < 0.001);
        let tiles = layout.tiles(WINDOW);
        assert!(tiles[0].1.w > 2 * tiles[1].1.w);
        layout.move_edge(WINDOW, Edge::Upright, 5000.0, 0.0);
        assert!(
            (layout.fx - 0.85).abs() < 0.001,
            "not pushed out of the window"
        );
        assert!((layout.fy - 0.5).abs() < 0.001, "the other line stays");
    }

    #[test]
    fn swapping_and_remembering() {
        let mut layout = Layout::for_displays(2, 0, None);
        layout.swap(0, 1);
        layout.fx = 0.3;
        assert_eq!(layout.tiles(WINDOW)[0].0, 1, "display 2 is on the left now");
        // Kept for the desk and found again.
        let kept: Layout = serde_json::from_str(&serde_json::to_string(&layout).unwrap()).unwrap();
        assert_eq!(Layout::for_displays(2, 0, Some(&kept)), layout);
        // The host has another display now: what was kept no longer fits.
        let fresh = Layout::for_displays(3, 0, Some(&kept));
        assert_eq!((fresh.order.as_slice(), fresh.fx), (&[0, 1, 2][..], 0.5));
    }

    #[test]
    fn a_display_is_asked_for_at_about_the_size_shown() {
        let half = Area {
            x: 0,
            y: 0,
            w: 959,
            h: 1000,
        };
        // 2560 × 1440 in a 959-wide tile is 37 %: asked for at 40.
        assert_eq!(scale_step(half, (2560, 1440)), 40);
        // 1920 × 1080 there is 50 %.
        assert_eq!(scale_step(half, (1920, 1080)), 50);
        // Never more than its own size, and unknown sizes are left alone.
        assert_eq!(scale_step(half, (800, 600)), 100);
        assert_eq!(scale_step(half, (0, 0)), 100);
    }

    #[test]
    fn the_size_is_read_from_a_display_s_words() {
        assert_eq!(size_in("2560 × 1440, main"), Some((2560, 1440)));
        assert_eq!(size_in("1920 × 1080"), Some((1920, 1080)));
        assert_eq!(size_in("a display"), None);
    }
}
