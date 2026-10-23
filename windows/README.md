# TriJello for Windows 11

The same cursed jello triangles, ported from ScreenCaptureKit + Metal to
Windows.Graphics.Capture + Direct3D 11. Every window becomes a wobbly jello mesh,
masked into a triangle, and HDR-boosted to your display's peak brightness.

## Build

Requires Windows 11 (Windows 10 2004+ works but shows a yellow capture border
around every window) and [Rust](https://rustup.rs).

```powershell
cd windows
cargo build --release
.\target\release\trijello.exe
```

No permissions to grant. The menu is behind the pink △ in the system tray
(you may need to click the ^ overflow arrow), where you can toggle HDR Boost,
Jello Physics and Triangles. **Ctrl+Alt+J** quits instantly if things get too cursed.

## Notes

- HDR Boost needs Windows HDR turned on (Settings → System → Display → Use HDR).
  Without it, everything renders at normal SDR brightness.
- The taskbar is left alone, so the tray menu always stays reachable.
- The wallpaper is drawn in "fill" mode from the image file; desktop icons aren't shown.
- Elevated (admin) windows can't be captured from a non-elevated app, so they stay
  hidden behind the overlay. Run TriJello as admin if you need them.
- Up to 16 windows are captured, frontmost first.
