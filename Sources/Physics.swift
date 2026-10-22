import CoreGraphics
import simd

let gridN = 12

/// A mass-spring "jello" grid laid over a window rect. Positions are in global
/// CoreGraphics points (top-left origin), same as CGWindowList bounds.
final class JelloBody {
    private(set) var pos: [SIMD2<Float>]
    private var vel: [SIMD2<Float>]
    private(set) var rect: CGRect
    var grabIndex: Int?
    private var resting = false

    static let uvs: [SIMD2<Float>] = (0..<gridN * gridN).map {
        SIMD2(Float($0 % gridN), Float($0 / gridN)) / Float(gridN - 1)
    }

    // (a, b, relative stiffness): structural + shear springs
    private static let springs: [(Int, Int, Float)] = {
        var s: [(Int, Int, Float)] = []
        for y in 0..<gridN {
            for x in 0..<gridN {
                let i = y * gridN + x
                if x + 1 < gridN { s.append((i, i + 1, 1)) }
                if y + 1 < gridN { s.append((i, i + gridN, 1)) }
                if x + 1 < gridN && y + 1 < gridN {
                    s.append((i, i + gridN + 1, 0.5))
                    s.append((i + 1, i + gridN, 0.5))
                }
            }
        }
        return s
    }()

    init(rect: CGRect, pop: Bool) {
        self.rect = rect
        let rest = Self.restPositions(rect)
        if pop {
            let c = SIMD2(Float(rect.midX), Float(rect.midY))
            pos = rest.map { c + ($0 - c) * 0.7 }
        } else {
            pos = rest
        }
        vel = Array(repeating: .zero, count: rest.count)
    }

    static func restPositions(_ r: CGRect) -> [SIMD2<Float>] {
        let o = SIMD2(Float(r.minX), Float(r.minY))
        let s = SIMD2(Float(r.width), Float(r.height))
        return uvs.map { o + $0 * s }
    }

    func grab(at p: SIMD2<Float>) {
        let rest = Self.restPositions(rect)
        grabIndex = rest.indices.min { distance_squared(rest[$0], p) < distance_squared(rest[$1], p) }
        resting = false
    }

    /// Advances the simulation; returns false when the mesh didn't move (nothing to redraw).
    @discardableResult
    func step(dt: Float, to newRect: CGRect, jello: Bool) -> Bool {
        if newRect != rect {
            rect = newRect
            resting = false
        }
        if grabIndex != nil { resting = false }
        if resting { return false }
        let rest = Self.restPositions(rect)
        guard jello else {
            pos = rest
            vel = vel.map { _ in .zero }
            resting = true
            return true
        }

        let substeps = 4
        let h = dt / Float(substeps)
        let kNeighbor: Float = 700
        let kHome: Float = grabIndex == nil ? 90 : 6
        let damping = exp(-5.5 * h)
        var force = [SIMD2<Float>](repeating: .zero, count: pos.count)

        for _ in 0..<substeps {
            for i in force.indices { force[i] = kHome * (rest[i] - pos[i]) }
            for (a, b, w) in Self.springs {
                let f = kNeighbor * w * ((pos[b] - pos[a]) - (rest[b] - rest[a]))
                force[a] += f
                force[b] -= f
            }
            for i in pos.indices {
                if i == grabIndex {
                    pos[i] = rest[i]
                    vel[i] = .zero
                    continue
                }
                vel[i] = (vel[i] + force[i] * h) * damping
                pos[i] += vel[i] * h
            }
        }

        if grabIndex == nil {
            var maxErr: Float = 0
            for i in pos.indices {
                maxErr = max(maxErr, length(pos[i] - rest[i]) + length(vel[i]) * 0.02)
            }
            if maxErr < 0.05 {
                pos = rest
                vel = vel.map { _ in .zero }
                resting = true
            }
        }
        return true
    }
}
