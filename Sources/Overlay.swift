import AppKit
import QuartzCore
import Metal

/// Full-screen, click-through, EDR-capable overlay that replaces the desktop's
/// look on one display. Sits above normal/floating windows but below the Dock
/// and menu bar.
final class OverlayWindow: NSWindow {
    static let level = Int(CGWindowLevelForKey(.modalPanelWindow))

    let displayID: CGDirectDisplayID
    let metalLayer = CAMetalLayer()

    init(screen: NSScreen, device: MTLDevice) {
        displayID = screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as! CGDirectDisplayID
        super.init(contentRect: screen.frame, styleMask: .borderless, backing: .buffered, defer: false)
        isOpaque = false
        backgroundColor = .clear
        hasShadow = false
        ignoresMouseEvents = true
        isReleasedWhenClosed = false
        level = NSWindow.Level(rawValue: Self.level)
        collectionBehavior = [.canJoinAllSpaces, .stationary, .ignoresCycle, .fullScreenAuxiliary]

        metalLayer.device = device
        metalLayer.pixelFormat = .rgba16Float
        metalLayer.wantsExtendedDynamicRangeContent = true
        metalLayer.colorspace = CGColorSpace(name: CGColorSpace.extendedLinearSRGB)
        metalLayer.isOpaque = false
        metalLayer.framebufferOnly = true
        metalLayer.contentsScale = screen.backingScaleFactor
        metalLayer.drawableSize = CGSize(width: screen.frame.width * screen.backingScaleFactor,
                                         height: screen.frame.height * screen.backingScaleFactor)

        let view = NSView(frame: NSRect(origin: .zero, size: screen.frame.size))
        view.layer = metalLayer
        view.wantsLayer = true
        contentView = view
        setFrame(screen.frame, display: true)
    }

    /// How far above SDR white this display can currently go (1.0 = no HDR).
    var headroom: Float {
        Float(screen?.maximumExtendedDynamicRangeColorComponentValue ?? 1)
    }
}
