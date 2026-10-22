import AppKit
import ScreenCaptureKit

struct WindowInfo {
    let id: CGWindowID
    let rect: CGRect   // global CG points, top-left origin
}

@MainActor
final class WindowManager: NSObject {
    var hdrEnabled = true { didSet { needsRedraw = true } }
    var jelloEnabled = true { didSet { needsRedraw = true } }
    var trianglesEnabled = true { didSet { needsRedraw = true } }

    private let renderer: Renderer
    private let textureCache: CVMetalTextureCache
    private var overlays: [OverlayWindow] = []
    private var displayLink: CADisplayLink?

    private var bodies: [CGWindowID: JelloBody] = [:]
    private var captures: [CGWindowID: StreamCapture] = [:]
    private var wallpapers: [CGDirectDisplayID: StreamCapture] = [:]
    /// Windows we couldn't (or chose not to) capture, with the time we may retry them.
    /// Without this, one uncapturable window forces a ScreenCaptureKit refresh every 0.3s forever.
    private var retryAfter: [CGWindowID: CFTimeInterval] = [:]

    private var lastTick: CFTimeInterval = 0
    private var lastRefresh: CFTimeInterval = 0
    private var refreshing = false
    private var wasMouseDown = false
    private var firstFrame = true
    private var needsRedraw = true
    private var lastFrameSignature = 0
    private var lastWindowIDs: [CGWindowID] = []
    private let myPID = getpid()
    // Every stream holds queueDepth full-size IOSurfaces, so these limits bound memory use.
    private let maxStreams = 16
    private let fullQualityWindows = 6   // frontmost windows get native resolution at 60fps
    private let backgroundFPS = 15
    private let wallpaperFPS = 10

    init(renderer: Renderer) {
        self.renderer = renderer
        var cache: CVMetalTextureCache?
        CVMetalTextureCacheCreate(nil, nil, renderer.device, nil, &cache)
        textureCache = cache!
        super.init()
        NotificationCenter.default.addObserver(self, selector: #selector(screensChanged),
                                               name: NSApplication.didChangeScreenParametersNotification, object: nil)
    }

    func start() {
        buildOverlays()
    }

    @objc private func screensChanged() {
        buildOverlays()
    }

    private func buildOverlays() {
        displayLink?.invalidate()
        overlays.forEach { $0.orderOut(nil) }
        wallpapers.values.forEach { $0.stop() }
        wallpapers.removeAll()

        overlays = NSScreen.screens.map { OverlayWindow(screen: $0, device: renderer.device) }
        overlays.forEach { $0.orderFrontRegardless() }
        if let view = overlays.first?.contentView {
            let link = view.displayLink(target: self, selector: #selector(tick))
            link.preferredFrameRateRange = CAFrameRateRange(minimum: 30, maximum: 60, preferred: 60)
            link.add(to: .main, forMode: .common)
            displayLink = link
        }
        lastRefresh = 0
        needsRedraw = true
    }

    // MARK: - Frame loop

    @objc private func tick() {
        let now = CACurrentMediaTime()
        let dt = Float(min(max(now - lastTick, 1.0 / 240), 1.0 / 30))
        lastTick = now

        let windows = currentWindows()
        let ids = Set(windows.map(\.id))

        for id in bodies.keys where !ids.contains(id) { bodies[id] = nil }
        for id in retryAfter.keys where !ids.contains(id) { retryAfter[id] = nil }
        for w in windows where bodies[w.id] == nil {
            bodies[w.id] = JelloBody(rect: w.rect, pop: !firstFrame)
        }
        firstFrame = false

        // Grab the jello where the user presses, so dragging wobbles from that point.
        let mouse = mouseLocationCG()
        let down = NSEvent.pressedMouseButtons & 1 != 0
        if down && !wasMouseDown, let hit = windows.first(where: { $0.rect.contains(mouse) }) {
            bodies[hit.id]?.grab(at: SIMD2(Float(mouse.x), Float(mouse.y)))
        } else if !down {
            for b in bodies.values { b.grabIndex = nil }
        }
        wasMouseDown = down

        var moved = false
        for w in windows {
            if bodies[w.id]?.step(dt: dt, to: w.rect, jello: jelloEnabled) == true { moved = true }
        }

        let scale = captureScale
        for (rank, w) in windows.enumerated() {
            guard let cap = captures[w.id], now - cap.lastResize > 0.25 else { continue }
            let q = quality(rank: rank, scale: scale)
            let want = CGSize(width: w.rect.width * q.scale, height: w.rect.height * q.scale)
            if abs(want.width - cap.configuredSize.width) > 2 || abs(want.height - cap.configuredSize.height) > 2
                || q.fps != cap.configuredFPS {
                cap.lastResize = now
                cap.reconfigure(size: want, fps: q.fps)
            }
        }

        let missing = windows.contains { captures[$0.id] == nil && (retryAfter[$0.id] ?? 0) <= now }
        if !refreshing && (now - lastRefresh > 5 || (missing && now - lastRefresh > 0.3)) {
            refreshCaptures(now: now)
        }

        // Skip the GPU work entirely when no mesh moved, no new frame arrived and
        // the window set is unchanged; the overlay keeps showing its last drawable.
        var signature = 0
        for c in captures.values { signature &+= c.frameCount }
        for c in wallpapers.values { signature &+= c.frameCount }
        let windowIDs = windows.map(\.id)
        guard needsRedraw || moved || signature != lastFrameSignature || windowIDs != lastWindowIDs else { return }
        needsRedraw = false
        lastFrameSignature = signature
        lastWindowIDs = windowIDs

        for overlay in overlays {
            let bounds = CGDisplayBounds(overlay.displayID)
            let origin = SIMD2(Float(bounds.minX), Float(bounds.minY))
            var items: [Renderer.Item] = []
            for w in windows.reversed() where w.rect.intersects(bounds) {
                guard let frame = captures[w.id]?.latest, let body = bodies[w.id] else { continue }
                let verts = zip(body.pos, JelloBody.uvs).map { Vertex(pos: $0 - origin, uv: $1) }
                items.append(Renderer.Item(frame: frame, vertices: verts))
            }
            renderer.render(overlay: overlay,
                            wallpaper: wallpapers[overlay.displayID]?.latest,
                            windows: items,
                            boost: hdrEnabled ? overlay.headroom : 1,
                            triangles: trianglesEnabled)
        }
    }

    /// On-screen app windows, front to back.
    private func currentWindows() -> [WindowInfo] {
        guard let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements],
                                                    kCGNullWindowID) as? [[String: Any]] else { return [] }
        return list.compactMap { d in
            let layer = d[kCGWindowLayer as String] as? Int ?? 0
            guard layer >= 0, layer < OverlayWindow.level,
                  (d[kCGWindowOwnerPID as String] as? pid_t) != myPID,
                  (d[kCGWindowAlpha as String] as? Double ?? 1) > 0.01,
                  let num = d[kCGWindowNumber as String] as? Int,
                  let bd = d[kCGWindowBounds as String],
                  let rect = CGRect(dictionaryRepresentation: bd as! CFDictionary),
                  rect.width >= 40, rect.height >= 40 else { return nil }
            return WindowInfo(id: CGWindowID(num), rect: rect)
        }
    }

    private func mouseLocationCG() -> CGPoint {
        let p = NSEvent.mouseLocation
        let primaryHeight = NSScreen.screens.first?.frame.height ?? 0
        return CGPoint(x: p.x, y: primaryHeight - p.y)
    }

    private var captureScale: CGFloat {
        NSScreen.screens.map(\.backingScaleFactor).max() ?? 2
    }

    /// Front windows are captured crisply; ones further back at 1x and a lower frame rate.
    private func quality(rank: Int, scale: CGFloat) -> (scale: CGFloat, fps: Int) {
        rank < fullQualityWindows ? (scale, 60) : (min(scale, 1), backgroundFPS)
    }

    // MARK: - Capture management

    private func refreshCaptures(now: CFTimeInterval) {
        refreshing = true
        lastRefresh = now
        Task { @MainActor in
            defer { refreshing = false }
            guard let content = try? await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true) else {
                return
            }
            let scale = captureScale
            let now = CACurrentMediaTime()

            for (id, cap) in captures where bodies[id] == nil || cap.failed {
                if cap.failed { retryAfter[id] = now + 10 }
                cap.stop()
                captures[id] = nil
            }

            // Walk windows front to back so the visible ones get streams first.
            let shareable = Dictionary(content.windows.map { ($0.windowID, $0) }, uniquingKeysWith: { a, _ in a })
            var started: [StreamCapture] = []
            for (rank, w) in currentWindows().enumerated() where captures[w.id] == nil && bodies[w.id] != nil {
                guard (retryAfter[w.id] ?? 0) <= now else { continue }
                guard let sw = shareable[w.id], captures.count < maxStreams else {
                    retryAfter[w.id] = now + 5
                    continue
                }
                let q = quality(rank: rank, scale: scale)
                let cap = StreamCapture(filter: SCContentFilter(desktopIndependentWindow: sw),
                                        pixelSize: CGSize(width: sw.frame.width * q.scale, height: sw.frame.height * q.scale),
                                        fps: q.fps, cache: textureCache)
                captures[w.id] = cap
                started.append(cap)
            }

            for overlay in overlays {
                if let existing = wallpapers[overlay.displayID], !existing.failed { continue }
                guard let display = content.displays.first(where: { $0.displayID == overlay.displayID }) else { continue }
                // Wallpaper + desktop icons live in negative window layers.
                let background = content.windows.filter { $0.windowLayer < 0 && $0.frame.intersects(display.frame) }
                let cap = StreamCapture(filter: SCContentFilter(display: display, including: background),
                                        pixelSize: CGSize(width: CGFloat(display.width) * scale,
                                                          height: CGFloat(display.height) * scale),
                                        fps: wallpaperFPS, cache: textureCache)
                wallpapers[overlay.displayID] = cap
                started.append(cap)
            }

            // Start all new streams concurrently instead of awaiting each one in turn.
            await withTaskGroup(of: Void.self) { group in
                for cap in started {
                    group.addTask { @MainActor in
                        do { try await cap.start() } catch { cap.failed = true }
                    }
                }
            }
        }
    }
}
