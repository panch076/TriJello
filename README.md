# TriJello

A macOS menu bar app that redraws your desktop live. Every window becomes a wobbly
jello mesh, can be masked into a triangle, and gets an HDR brightness boost on EDR displays.

Built with ScreenCaptureKit and Metal.

## Build

Requires macOS 14+ on Apple Silicon and the Xcode command line tools.

```sh
./build.sh
open TriJello.app
```

On first launch, grant Screen Recording permission in System Settings → Privacy & Security →
Screen & System Audio Recording, then relaunch. The app's menu sits behind the △ in the
menu bar, where you can toggle HDR Boost, Jello Physics and Triangles.
