// A presenter with side effects is not a presenter (contract §3.0). Every line
// below must be named by `presenter_failures`; the whole thesis of contract v2
// is that none of them can exist in a presenter file.
import Foundation

struct NotAPresenter {
    // 1. Moving the player.
    func stop(_ player: AVPlayer) { player.pause() }
    func start(_ player: AVPlayer) { player.play() }
    func jump(_ player: AVPlayer) { player.seek(to: .zero) }
    func ready(_ item: AVPlayerItem) { item.prepare() }
    func swap(_ player: AVPlayer) { player.replaceCurrentItem(with: nil) }

    // 2. A timer of its own, in each spelling that creates one.
    func arm() {
        Timer.scheduledTimer(withTimeInterval: 1, repeats: false) { _ in }
        DispatchQueue.main.asyncAfter(deadline: .now() + 1) { }
        Task { try? await Task.sleep(for: .seconds(1)) }
    }

    // 3. Reporting to the control plane.
    func report(_ playbackControl: PlaybackControlSession) {
        notifyPlaybackControl("failed", nil)
        reportControlEvidence(nil)
    }
}
