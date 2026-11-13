# Illumination app icon

Two overlapping screens (40% overlap), navy outlines on a soft blue tile: the screen behind is
the remote one, the screen in front is yours.

| Role | Colour |
|---|---|
| Tile background | `#DDE6F6` |
| Outlines, outer edge, inner keyline | `#1E2A4A` (navy) |
| Back screen (remote) | `#FFE012` |
| Front screen (yours) | `#FFF68D` (main colour) |

These four are also the base colours of the app's interface.

## Files

- `illumination.ico`: Windows icon with 10 sizes (16, 20, 24, 32, 40, 48, 64, 96, 128, 256).
  `crates/illum/assets/illumination.ico` is the copy the build puts into `illum.exe`.
- `svg/illumination.svg`: master drawing, for 128 px and up.
- `svg/illumination-64.svg`, `-32.svg`, `-16.svg`: hand-tuned small versions (thicker lines; the
  32 and 16 drop the inner keyline). The PNGs and the ICO already use these at the matching
  sizes. Don't scale the master down to small sizes; use these.
- `png/illumination-<size>.png`: 16, 20, 24, 32, 40, 48, 64, 96, 128, 256, 512, 1024.

## Where it is used

- **`illum.exe`** (Explorer, taskbar): `crates/illum/build.rs` embeds the ICO as resource 1.
- **Windows of the app**: `illum_ui::icon::with_icon` loads that resource on Windows and falls
  back to the built-in 64 px PNG elsewhere.
- **Tray icon** (later): the same ICO; Windows picks the 16/20/24 px images from it.
