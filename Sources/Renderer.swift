import Metal
import CoreGraphics

struct Vertex {
    var pos: SIMD2<Float>
    var uv: SIMD2<Float>
}

struct Uniforms {
    var viewport: SIMD2<Float>
    var uvMax: SIMD2<Float>
    var boost: Float
    var triangle: Float
}

private let shaderSource = """
#include <metal_stdlib>
using namespace metal;

struct Vertex { float2 pos; float2 uv; };
struct Uniforms { float2 viewport; float2 uvMax; float boost; float triangle; };
struct VOut { float4 position [[position]]; float2 uv; };

vertex VOut vmain(uint vid [[vertex_id]],
                  const device Vertex* v [[buffer(0)]],
                  constant Uniforms& u [[buffer(1)]]) {
    VOut o;
    float2 p = v[vid].pos / u.viewport;
    o.position = float4(p.x * 2.0 - 1.0, 1.0 - p.y * 2.0, 0.0, 1.0);
    o.uv = v[vid].uv;
    return o;
}

fragment float4 fmain(VOut in [[stage_in]],
                      texture2d<float> tex [[texture(0)]],
                      constant Uniforms& u [[buffer(1)]]) {
    constexpr sampler s(filter::linear, address::clamp_to_edge);
    float4 c = tex.sample(s, in.uv * u.uvMax);   // linear (bgra8Unorm_srgb), premultiplied
    float mask = 1.0;
    if (u.triangle > 0.5) {
        // Apex at top-center, base along the bottom edge. d > 0 inside.
        float d = in.uv.y * 0.5 - abs(in.uv.x - 0.5);
        float w = max(fwidth(d), 1e-5);
        mask = smoothstep(-w, w, d);
    }
    return float4(c.rgb * u.boost, c.a) * mask;
}
"""

final class Renderer {
    let device: MTLDevice
    let queue: MTLCommandQueue
    private let pipeline: MTLRenderPipelineState
    private let gridIndices: MTLBuffer
    private let gridIndexCount: Int
    private let quadIndices: MTLBuffer

    init() throws {
        device = MTLCreateSystemDefaultDevice()!
        queue = device.makeCommandQueue()!
        let lib = try device.makeLibrary(source: shaderSource, options: nil)
        let desc = MTLRenderPipelineDescriptor()
        desc.vertexFunction = lib.makeFunction(name: "vmain")
        desc.fragmentFunction = lib.makeFunction(name: "fmain")
        let att = desc.colorAttachments[0]!
        att.pixelFormat = .rgba16Float
        att.isBlendingEnabled = true
        att.sourceRGBBlendFactor = .one
        att.sourceAlphaBlendFactor = .one
        att.destinationRGBBlendFactor = .oneMinusSourceAlpha
        att.destinationAlphaBlendFactor = .oneMinusSourceAlpha
        pipeline = try device.makeRenderPipelineState(descriptor: desc)

        var idx: [UInt16] = []
        for y in 0..<(gridN - 1) {
            for x in 0..<(gridN - 1) {
                let i = UInt16(y * gridN + x), n = UInt16(gridN)
                idx += [i, i + 1, i + n, i + 1, i + n + 1, i + n]
            }
        }
        gridIndexCount = idx.count
        gridIndices = device.makeBuffer(bytes: idx, length: idx.count * 2)!
        let q: [UInt16] = [0, 1, 2, 1, 3, 2]
        quadIndices = device.makeBuffer(bytes: q, length: q.count * 2)!
    }

    struct Item {
        let frame: FrameTexture
        let vertices: [Vertex]   // gridN*gridN, in display-local points
    }

    func render(overlay: OverlayWindow, wallpaper: FrameTexture?, windows: [Item], boost: Float, triangles: Bool) {
        guard let drawable = overlay.metalLayer.nextDrawable(),
              let cb = queue.makeCommandBuffer() else { return }
        let rpd = MTLRenderPassDescriptor()
        rpd.colorAttachments[0].texture = drawable.texture
        rpd.colorAttachments[0].loadAction = .clear
        rpd.colorAttachments[0].clearColor = MTLClearColor(red: 0, green: 0, blue: 0, alpha: 0)
        rpd.colorAttachments[0].storeAction = .store
        guard let enc = cb.makeRenderCommandEncoder(descriptor: rpd) else { return }
        enc.setRenderPipelineState(pipeline)

        let bounds = CGDisplayBounds(overlay.displayID)
        let viewport = SIMD2(Float(bounds.width), Float(bounds.height))

        // Without the wallpaper we can't hide the real windows' corners, so draw
        // nothing and let the real desktop show through untouched.
        if let wallpaper {
            let quad = [
                Vertex(pos: [0, 0], uv: [0, 0]),
                Vertex(pos: [viewport.x, 0], uv: [1, 0]),
                Vertex(pos: [0, viewport.y], uv: [0, 1]),
                Vertex(pos: viewport, uv: [1, 1]),
            ]
            draw(enc, frame: wallpaper, vertices: quad, indices: quadIndices, count: 6,
                 uniforms: Uniforms(viewport: viewport, uvMax: wallpaper.uvMax, boost: 1, triangle: 0))

            for item in windows {
                draw(enc, frame: item.frame, vertices: item.vertices, indices: gridIndices, count: gridIndexCount,
                     uniforms: Uniforms(viewport: viewport, uvMax: item.frame.uvMax, boost: boost,
                                        triangle: triangles ? 1 : 0))
            }
        }

        enc.endEncoding()
        cb.present(drawable)
        cb.commit()
    }

    private func draw(_ enc: MTLRenderCommandEncoder, frame: FrameTexture, vertices: [Vertex],
                      indices: MTLBuffer, count: Int, uniforms: Uniforms) {
        var u = uniforms
        vertices.withUnsafeBytes { enc.setVertexBytes($0.baseAddress!, length: $0.count, index: 0) }
        enc.setVertexBytes(&u, length: MemoryLayout<Uniforms>.stride, index: 1)
        enc.setFragmentBytes(&u, length: MemoryLayout<Uniforms>.stride, index: 1)
        enc.setFragmentTexture(frame.texture, index: 0)
        enc.drawIndexedPrimitives(type: .triangle, indexCount: count, indexType: .uint16,
                                  indexBuffer: indices, indexBufferOffset: 0)
    }
}
