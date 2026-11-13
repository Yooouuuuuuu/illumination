// SPDX-License-Identifier: GPL-3.0-only
//! The app icon on a window (`assets/icon`).
//!
//! On Windows it comes from the icon resource the `illum` build puts into the executable, so
//! the title bar and the taskbar each get the size drawn for them. Elsewhere, and in a program
//! without that resource, the window gets the 64-pixel drawing built into this crate.

use winit::window::{Icon, WindowAttributes};

/// The resource `crates/illum/build.rs` stores the icon under (`winresource::set_icon`).
#[cfg(windows)]
pub const RESOURCE_ID: u16 = 1;

const PNG_64: &[u8] = include_bytes!("../../../assets/icon/png/illumination-64.png");

/// A PNG as 8-bit RGBA: `(pixels, width, height)`.
fn decode_png(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(
        png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info().ok()?;
    let mut pixels = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut pixels).ok()?;
    if info.color_type != png::ColorType::Rgba {
        return None;
    }
    pixels.truncate(info.buffer_size());
    Some((pixels, info.width, info.height))
}

fn built_in() -> Option<Icon> {
    let (pixels, width, height) = decode_png(PNG_64)?;
    Icon::from_rgba(pixels, width, height).ok()
}

/// Gives a window the app icon.
pub fn with_icon(attrs: WindowAttributes) -> WindowAttributes {
    #[cfg(windows)]
    {
        use winit::platform::windows::{IconExtWindows, WindowAttributesExtWindows};
        // `None`: Windows picks the size it wants for each place from the ones in the resource.
        if let (Ok(small), Ok(big)) = (
            Icon::from_resource(RESOURCE_ID, None),
            Icon::from_resource(RESOURCE_ID, None),
        ) {
            return attrs
                .with_window_icon(Some(small))
                .with_taskbar_icon(Some(big));
        }
    }
    attrs.with_window_icon(built_in())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_icon_decodes() {
        let (pixels, width, height) = decode_png(PNG_64).expect("the PNG decodes");
        assert_eq!((width, height), (64, 64));
        assert_eq!(pixels.len(), 64 * 64 * 4);
        // The tile's corners are rounded: the very corner is transparent, the middle is not.
        assert_eq!(pixels[3], 0);
        assert_eq!(pixels[(32 * 64 + 32) * 4 + 3], 255);
    }
}
