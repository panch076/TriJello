import ScreenCaptureKit
import CoreMedia
import CoreVideo
import Metal

/// Latest captured frame of a window (or wallpaper), ready to sample in Metal.
final class FrameTexture {
    let texture: MTLTexture
    let cvTexture: CVMetalTexture   // keeps the IOSurface-backed texture alive
    let pixelBuffer: CVPixelBuffer
    let uvMax: SIMD2<Float>         // portion of the buffer actually covered by content

    init(texture: MTLTexture, cvTexture: CVMetalTexture, pixelBuffer: CVPixelBuffer, uvMax: SIMD2<Float>) {
        self.texture = texture
        self.cvTexture = cvTexture
        self.pixelBuffer = pixelBuffer
        self.uvMax = uvMax
    }
}

/// Streams one SCContentFilter into Metal textures.
final class StreamCapture: NSObject, SCStreamOutput, SCStreamDelegate {
    static let queue = DispatchQueue(label: "trijello.capture", qos: .userInteractive)

    private var stream: SCStream!
    private let cache: CVMetalTextureCache
    private let lock = NSLock()
    private var _latest: FrameTexture?
    private var _failed = false
    private var _frameCount = 0
    private(set) var configuredSize: CGSize
    private(set) var configuredFPS: Int
    var lastResize: CFTimeInterval = 0

    var latest: FrameTexture? { lock.withLock { _latest } }
    /// Bumped on every delivered frame, so the renderer can skip redraws when nothing changed.
    var frameCount: Int { lock.withLock { _frameCount } }
    var failed: Bool {
        get { lock.withLock { _failed } }
        set { lock.withLock { _failed = newValue } }
    }

    init(filter: SCContentFilter, pixelSize: CGSize, fps: Int, cache: CVMetalTextureCache) {
        self.cache = cache
        self.configuredSize = pixelSize
        self.configuredFPS = fps
        super.init()
        stream = SCStream(filter: filter, configuration: Self.makeConfig(pixelSize, fps: fps), delegate: self)
    }

    func start() async throws {
        try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: Self.queue)
        try await stream.startCapture()
    }

    func stop() {
        let s = stream
        Task { try? await s?.stopCapture() }
    }

    func reconfigure(size: CGSize, fps: Int) {
        configuredSize = size
        configuredFPS = fps
        stream.updateConfiguration(Self.makeConfig(size, fps: fps)) { _ in }
    }

    private static func makeConfig(_ size: CGSize, fps: Int) -> SCStreamConfiguration {
        let c = SCStreamConfiguration()
        c.width = max(2, Int(size.width.rounded()))
        c.height = max(2, Int(size.height.rounded()))
        c.pixelFormat = kCVPixelFormatType_32BGRA
        c.colorSpaceName = CGColorSpace.sRGB
        c.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(fps))
        c.queueDepth = 3   // minimum allowed; each buffer is a full-size IOSurface
        c.showsCursor = false
        c.ignoreShadowsSingleWindow = true
        return c
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sb: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, sb.isValid,
              let attachments = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let info = attachments.first,
              let rawStatus = info[.status] as? Int,
              SCFrameStatus(rawValue: rawStatus) == .complete,
              let pb = sb.imageBuffer else { return }

        let w = CVPixelBufferGetWidth(pb), h = CVPixelBufferGetHeight(pb)
        var cvTex: CVMetalTexture?
        CVMetalTextureCacheCreateTextureFromImage(nil, cache, pb, nil, .bgra8Unorm_srgb, w, h, 0, &cvTex)
        guard let cvTex, let tex = CVMetalTextureGetTexture(cvTex) else { return }

        // The content may not fill the whole buffer mid-resize; sample only the covered part.
        var uvMax = SIMD2<Float>(1, 1)
        if let dict = info[.contentRect], let rect = CGRect(dictionaryRepresentation: dict as! CFDictionary) {
            let scale = (info[.scaleFactor] as? CGFloat) ?? 1
            let u = Float(rect.width * scale) / Float(w), v = Float(rect.height * scale) / Float(h)
            if u > 0.05 && u <= 1.05 { uvMax.x = min(1, u) }
            if v > 0.05 && v <= 1.05 { uvMax.y = min(1, v) }
        }

        let frame = FrameTexture(texture: tex, cvTexture: cvTex, pixelBuffer: pb, uvMax: uvMax)
        lock.withLock {
            _latest = frame
            _frameCount &+= 1
        }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        failed = true
    }
}
