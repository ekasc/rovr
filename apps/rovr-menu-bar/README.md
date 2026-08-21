# Rovr Menu Bar

Tiny macOS status item. It polls `rovr doctor` every 5 seconds and shows `◉` when the daemon responds and `◐` when it does not. Menu offers Doctor and Events.

Build:

```
swift build -c release
.build/release/RovrMenuBar
```

This is the M4 Swift diagnostics UI. It is intentionally small. No private APIs, no window management, just `rovr` CLI over the Unix socket. A richer view can subscribe to `rovr subscribe` later.
