// SPDX-License-Identifier: GPL-3.0-only
//! The app's icon in the system tray.
//!
//! The app goes on while its window is hidden: this icon is where it lives then. A click
//! opens the window; its menu opens it, shows who is connected here, or closes the app. The
//! icon turns yellow while a desk is connected, so that is seen without opening anything.

/// What the person asked for at the tray.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Open,
    /// Open on the list of desks connected here.
    Connected,
    Quit,
}

#[cfg(any(windows, target_os = "macos"))]
mod imp {
    use super::TrayAction;
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };

    const PNG_64: &[u8] = include_bytes!("../../../assets/icon/png/illumination-64.png");

    pub struct Tray {
        icon: TrayIcon,
        connected: MenuItem,
        plain: Option<Icon>,
        busy: Option<Icon>,
        count: usize,
    }

    /// The app icon; with `busy`, its pale tile turned the strong yellow.
    fn picture(busy: bool) -> Option<Icon> {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(PNG_64));
        decoder.set_transformations(
            png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
        );
        let mut reader = decoder.read_info().ok()?;
        let mut pixels = vec![0; reader.output_buffer_size()?];
        let info = reader.next_frame(&mut pixels).ok()?;
        pixels.truncate(info.buffer_size());
        if busy {
            for px in pixels.chunks_exact_mut(4) {
                // The tile is the one pale blue in the picture (0xDDE6F6).
                let near = |a: u8, b: u8| a.abs_diff(b) < 24;
                if near(px[0], 0xDD) && near(px[1], 0xE6) && near(px[2], 0xF6) {
                    px[..3].copy_from_slice(&[0xFF, 0xE0, 0x12]);
                }
            }
        }
        Icon::from_rgba(pixels, info.width, info.height).ok()
    }

    impl Tray {
        /// Puts the icon in the tray. `on_action` is called for a click or a menu choice, on
        /// whatever thread the system reports it. Must be called on the thread that runs the
        /// window's event loop, once that loop runs.
        pub fn new(on_action: impl Fn(TrayAction) + Send + Sync + 'static) -> Option<Tray> {
            let open = MenuItem::new("Open", true, None);
            let connected = MenuItem::new("Connected here: 0", true, None);
            let quit = MenuItem::new("Quit", true, None);
            let menu = Menu::new();
            menu.append_items(&[&open, &connected, &PredefinedMenuItem::separator(), &quit])
                .ok()?;
            let plain = picture(false);
            let mut builder = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .with_tooltip("Illumination");
            if let Some(icon) = plain.clone() {
                builder = builder.with_icon(icon);
            }
            let icon = builder.build().ok()?;
            let on_action = std::sync::Arc::new(on_action);
            let (open, shown, quit) =
                (open.id().clone(), connected.id().clone(), quit.id().clone());
            let chosen = on_action.clone();
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                if event.id == open {
                    chosen(TrayAction::Open);
                } else if event.id == shown {
                    chosen(TrayAction::Connected);
                } else if event.id == quit {
                    chosen(TrayAction::Quit);
                }
            }));
            TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    on_action(TrayAction::Open);
                }
            }));
            Some(Tray {
                icon,
                connected,
                plain,
                busy: picture(true),
                count: 0,
            })
        }

        /// Says how many desks are connected here: in the menu, the tooltip and the icon.
        pub fn set_connected(&mut self, count: usize) {
            if count == self.count {
                return;
            }
            self.count = count;
            self.connected.set_text(format!("Connected here: {count}"));
            let tip = match count {
                0 => "Illumination".to_string(),
                1 => "Illumination: 1 desk connected here".to_string(),
                n => format!("Illumination: {n} desks connected here"),
            };
            let _ = self.icon.set_tooltip(Some(tip));
            let picture = if count > 0 { &self.busy } else { &self.plain };
            let _ = self.icon.set_icon(picture.clone());
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    use super::TrayAction;

    /// No tray on this system yet: the window is the app.
    pub struct Tray;

    impl Tray {
        pub fn new(_on_action: impl Fn(TrayAction) + Send + Sync + 'static) -> Option<Tray> {
            None
        }

        pub fn set_connected(&mut self, _count: usize) {}
    }
}

pub use imp::Tray;
