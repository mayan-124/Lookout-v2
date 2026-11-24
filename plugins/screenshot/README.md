# Lookout Screenshot Plugin (`dev.lookout.screenshot`)

Screen capture, scrolling capture, and screen recording for Lookout.

## Capabilities

- **Screen Capture** (`Ctrl+Alt+S`): Region selection overlay, active window capture, and full virtual desktop capture across multi-monitor setups with per-monitor DPI scaling. Includes full annotation editor (arrows, rectangles, ellipses, lines, highlighters, system fonts text, auto-increment step markers, pixel-level blur redaction, and crop).
- **Scrolling Capture** (`Ctrl+Alt+Shift+S`): Long page capture via vertical overlap stitching, automatic sticky header/footer exclusion, smooth scrolling settle checks, and Page Down fallback with "window did not scroll" notifications.
- **Screen Recording** (`Ctrl+Alt+R`): Screen area, window, or full-screen video recording via Windows Media Foundation H.264 MP4 SinkWriter with hardware-accelerated transforms and automatic software fallback. Includes floating control bar, click highlight rings, post-recording result card, and pure-Rust animated GIF export. Hard caps at 60 minutes or 2 GB (with warnings at 55 minutes or 1.9 GB).

## Capture Engine Limitations (GDI Capture)

The screen recording and screenshot engine currently uses Win32 GDI `BitBlt` with `CAPTUREBLT` and software cursor compositing:

- **GPU / DRM Content**: Black frames are possible when attempting to capture hardware-accelerated overlays, exclusive fullscreen games, or DRM-protected video surfaces (e.g., Netflix, protected browser video streams).
- **CPU Overhead**: Frame copying and compositing are processed through system memory (GDI DC transfers), resulting in high CPU utilization at 1080p 60 fps or higher resolutions/framerates. 30 fps is recommended for general recording.
