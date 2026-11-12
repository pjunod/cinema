// dv_display_check.swift FILE.mp4 -- macOS only; build with: swiftc -O -o dv_display_check dv_display_check.swift
// render an AVPlayerLayer offscreen with CARenderer into an rgba16Float Metal texture (EDR on),
// and print the mean linear R, G, B of a central region, to see whether the display-path DV processing applies the RPU mapping.
import AVFoundation
import Metal
import QuartzCore
let url = URL(fileURLWithPath: CommandLine.arguments[1])
let dev = MTLCreateSystemDefaultDevice()!
let W = 960, H = 540
let td = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .rgba16Float, width: W, height: H, mipmapped: false)
td.usage = [.renderTarget, .shaderRead]; td.storageMode = .managed
let tex = dev.makeTexture(descriptor: td)!
let cs = CGColorSpace(name: CGColorSpace.extendedLinearITUR_2020)!
let renderer = CARenderer(mtlTexture: tex, options: [kCARendererColorSpace as String: cs])
let player = AVPlayer(url: url); player.isMuted = true
let pl = AVPlayerLayer(player: player); pl.frame = CGRect(x: 0, y: 0, width: W, height: H)
pl.wantsExtendedDynamicRangeContent = true
let root = CALayer(); root.frame = pl.frame; root.addSublayer(pl); root.wantsExtendedDynamicRangeContent = true
renderer.layer = root; renderer.bounds = root.frame
player.play()
let t0 = Date(); var best = ""
while Date().timeIntervalSince(t0) < 3.0 {
    RunLoop.current.run(until: Date().addingTimeInterval(0.04))
    CATransaction.flush()
    renderer.beginFrame(atTime: CACurrentMediaTime(), timeStamp: nil)
    renderer.addUpdate(renderer.bounds); renderer.render(); renderer.endFrame()
}
let q = dev.makeCommandQueue()!; let cb = q.makeCommandBuffer()!; let bl = cb.makeBlitCommandEncoder()!; bl.synchronize(resource: tex); bl.endEncoding(); cb.commit(); cb.waitUntilCompleted()
var px = [Float16](repeating: 0, count: W * H * 4)
tex.getBytes(&px, bytesPerRow: W * 8, from: MTLRegionMake2D(0, 0, W, H), mipmapLevel: 0)
var s = 0.0, n = 0, nz = 0
for y in stride(from: H / 4, to: 3 * H / 4, by: 2) { for x in stride(from: W / 4, to: 3 * W / 4, by: 2) { let v = Double(px[(y * W + x) * 4]); s += v; n += 1; if v != 0 { nz += 1 } } }
var g = 0.0, b = 0.0
for y in stride(from: H / 4, to: 3 * H / 4, by: 2) {
    for x in stride(from: W / 4, to: 3 * W / 4, by: 2) { g += Double(px[(y * W + x) * 4 + 1]); b += Double(px[(y * W + x) * 4 + 2]) }
}
print("layer-render t=\(String(format: "%.2f", player.currentTime().seconds)) R-mean=\(s / Double(n)) G-mean=\(g / Double(n)) B-mean=\(b / Double(n)) nonzero=\(nz)/\(n)")
