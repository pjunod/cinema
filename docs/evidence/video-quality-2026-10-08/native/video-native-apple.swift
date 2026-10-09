// Finite AVPlayer delivery qualification for loopback Plurx-produced HLS.
// This companion application runs only in an owned simulator. It does not
// claim physical panel/audio-output acceptance or modify the shipping client.
import UIKit
import AVFoundation
import CoreVideo
import QuartzCore

@main
final class ProbeApp: UIResponder, UIApplicationDelegate {
    var window: UIWindow?
    func application(_ application: UIApplication,
                     didFinishLaunchingWithOptions options: [UIApplication.LaunchOptionsKey: Any]?) -> Bool {
        window = UIWindow(frame: UIScreen.main.bounds)
        window?.rootViewController = ProbeView()
        window?.makeKeyAndVisible()
        return true
    }
}

final class ProbeView: UIViewController {
    let player = AVPlayer()
    var layer: AVPlayerLayer!
    var output: AVPlayerItemVideoOutput?
    var timer: Timer?
    var observations = [[String: Any]]()
    var phases = [[String: Any]]()
    let targets: [(String, Double)] = [("start", 0), ("forward", 12), ("backward", 3), ("restart", 8)]
    var phase = -1
    var phaseStart = 0.0
    var firstTime: Double?
    var lastTime: Double?
    var phaseFrames = 0
    var initialFloorExclusions = 0
    var totalInitialFloorExclusions = 0
    var readyToCollect = false
    var done = false
    var url: URL!
    var began = CACurrentMediaTime()

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black
        guard let text = ProcessInfo.processInfo.environment["QUAL_URL"],
              let value = URL(string: text),
              value.scheme == "http", value.host == "127.0.0.1" else {
            finish("QUAL_URL must be an owned loopback HTTP asset")
            return
        }
        url = value
        layer = AVPlayerLayer(player: player)
        layer.videoGravity = .resizeAspect
        view.layer.addSublayer(layer)
        timer = Timer.scheduledTimer(withTimeInterval: 0.01, repeats: true) { [weak self] _ in self?.poll() }
        advance()
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        layer?.frame = view.bounds
    }

    func attach() {
        let item = AVPlayerItem(url: url)
        let sink = AVPlayerItemVideoOutput(pixelBufferAttributes: [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA
        ])
        item.add(sink)
        output = sink
        player.replaceCurrentItem(with: item)
    }

    func advance() {
        readyToCollect = false
        player.pause()
        phase += 1
        if phase == targets.count { finish(nil); return }
        phaseStart = CACurrentMediaTime()
        firstTime = nil
        lastTime = nil
        phaseFrames = 0
        initialFloorExclusions = 0
        if phase == 0 || targets[phase].0 == "restart" { attach() }
        let seek = CMTime(seconds: targets[phase].1, preferredTimescale: 600)
        player.seek(to: seek, toleranceBefore: .zero, toleranceAfter: .zero) { [weak self] ok in
            DispatchQueue.main.async {
                guard let self = self, !self.done else { return }
                if !ok { self.finish("seek completion failed"); return }
                self.readyToCollect = true
                self.player.play()
            }
        }
    }

    func fingerprint(_ buffer: CVPixelBuffer) -> String {
        CVPixelBufferLockBaseAddress(buffer, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(buffer, .readOnly) }
        guard let base = CVPixelBufferGetBaseAddress(buffer) else { return "unmapped" }
        let bytes = base.assumingMemoryBound(to: UInt8.self)
        let width = CVPixelBufferGetWidth(buffer), height = CVPixelBufferGetHeight(buffer)
        let stride = CVPixelBufferGetBytesPerRow(buffer)
        var hash: UInt64 = 14695981039346656037
        for y in Swift.stride(from: 0, to: height, by: max(1, height / 32)) {
            for x in Swift.stride(from: 0, to: width, by: max(1, width / 32)) {
                for channel in 0..<3 {
                    hash = (hash ^ UInt64(bytes[y * stride + x * 4 + channel])) &* 1099511628211
                }
            }
        }
        return String(hash, radix: 16)
    }

    func poll() {
        guard !done, phase >= 0, phase < targets.count else { return }
        let now = CACurrentMediaTime()
        if now - began > 100 || now - phaseStart > 20 {
            finish("bounded phase deadline exceeded")
            return
        }
        guard let item = player.currentItem else { finish("current item disappeared"); return }
        if item.status == .failed { finish(item.error?.localizedDescription ?? "item failed"); return }
        guard readyToCollect, let sink = output,
              view.window != nil, !view.isHidden, layer.isReadyForDisplay else { return }
        let requested = sink.itemTime(forHostTime: now)
        guard sink.hasNewPixelBuffer(forItemTime: requested) else { return }
        var actual = CMTime.invalid
        guard let buffer = sink.copyPixelBuffer(forItemTime: requested, itemTimeForDisplay: &actual) else { return }
        let media = actual.seconds
        guard media.isFinite else { finish("nonfinite frame timestamp"); return }
        // A ready layer alone may retain an old picture across a seek. The
        // decoded output's item time must belong to the requested new epoch.
        if firstTime == nil && media < targets[phase].1 - 0.1 {
            initialFloorExclusions += 1
            totalInitialFloorExclusions += 1
            return
        }
        if let last = lastTime, media < last - 0.001 {
            finish("presentation timestamp reversed within a phase")
            return
        }
        let clock = player.currentTime().seconds
        let audioTracks = item.tracks.filter { $0.assetTrack?.mediaType == .audio }.count
        observations.append([
            "phase": targets[phase].0, "media_seconds": media,
            "player_clock_seconds": clock, "wall_since_phase_seconds": now - phaseStart,
            "visible_ready_layer": true, "audio_tracks": audioTracks,
            "width": CVPixelBufferGetWidth(buffer), "height": CVPixelBufferGetHeight(buffer),
            "sampled_pixel_fingerprint": fingerprint(buffer)
        ])
        if firstTime == nil { firstTime = media }
        lastTime = media
        phaseFrames += 1
        if let first = firstTime, media - first >= 2.5 && phaseFrames >= 30 {
            let rows = observations.filter { ($0["phase"] as? String) == targets[phase].0 }
            let deltas = rows.compactMap { row -> Double? in
                guard let m = row["media_seconds"] as? Double,
                      let c = row["player_clock_seconds"] as? Double else { return nil }
                return abs(m - c)
            }
            let maximum = deltas.max() ?? .infinity
            let distinct = Set(rows.compactMap { $0["sampled_pixel_fingerprint"] as? String }).count
            guard audioTracks > 0, maximum < 0.25, distinct > 1 else {
                finish("audio-track/clock/moving-picture contract failed")
                return
            }
            phases.append([
                "phase": targets[phase].0, "frames_observed": phaseFrames,
                "initial_floor_exclusions": initialFloorExclusions,
                "first_frame_seconds": rows.first?["wall_since_phase_seconds"] ?? 0,
                "maximum_video_to_player_clock_seconds": maximum,
                "distinct_pixel_fingerprints": distinct, "result": "pass"
            ])
            advance()
        }
    }

    func finish(_ failure: String?) {
        guard !done else { return }
        done = true
        timer?.invalidate()
        player.pause()
        player.replaceCurrentItem(with: nil)
        let record: [String: Any] = [
            "schema": 1, "runtime": "AVPlayer on tvOS simulator",
            "os": ProcessInfo.processInfo.operatingSystemVersionString,
            "case": ProcessInfo.processInfo.environment["QUAL_CASE"] ?? "unnamed",
            "result": failure == nil ? "pass" : "fail",
            "failure": failure ?? NSNull(), "phases": phases, "observations": observations,
            "initial_floor_exclusions": totalInitialFloorExclusions,
            "scope": "Actual native decode, visible simulator layer readiness, timestamp progression, forward/backward seek and fresh-item restart of Plurx HLS. Video-to-player clock and audio-track presence are measured; physical audio sync and physical display are not measured."
        ]
        do {
            let data = try JSONSerialization.data(withJSONObject: record, options: [.prettyPrinted, .sortedKeys])
            let folder = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
            try data.write(to: folder.appendingPathComponent("qualification.json"), options: .atomic)
            print("QUALIFICATION_FINISHED \(failure == nil ? "pass" : "fail")")
        } catch { print("QUALIFICATION_WRITE_FAILED \(error)") }
    }
}
