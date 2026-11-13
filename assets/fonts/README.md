# Fonts

The app's interface fonts, embedded in the binary by `illum_ui::app::fonts`. All three are under
the SIL Open Font License 1.1 (the `OFL-*.txt` files here). These are the static Latin files
from Fontsource (<https://fontsource.org>), unmodified.

| File | Family | Used for |
|---|---|---|
| `Inter-Regular.ttf`, `Inter-SemiBold.ttf` | Inter | text, labels, buttons |
| `SpaceGrotesk-Bold.ttf` | Space Grotesk | headings, the main button |
| `JetBrainsMono-Medium.ttf` | JetBrains Mono | addresses and keys |

They hold Latin only. For Chinese, Japanese and Korean the app adds a font from the system
behind them when it finds one.
