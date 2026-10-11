import AVFoundation
import AVKit
import Foundation
import UIKit

enum DisplayCriteriaDecision: Equatable, Sendable {
    case apply
    case skip

    nonisolated static func decide(
        matchingEnabled: Bool,
        itemIsCurrent: Bool,
        openIsCurrent: Bool,
        itemIsReady: Bool
    ) -> Self {
        matchingEnabled && itemIsCurrent && openIsCurrent && itemIsReady ? .apply : .skip
    }
}

#if os(tvOS)
@MainActor
enum PlaybackDisplayCriteria {
    static func activeManager() -> AVDisplayManager? {
        UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .flatMap(\.windows)
            .first(where: \.isKeyWindow)?
            .avDisplayManager
    }

    @discardableResult
    static func apply(
        item: AVPlayerItem,
        itemIsCurrent: Bool,
        openIsCurrent: Bool
    ) -> Bool {
        guard let manager = activeManager(),
              DisplayCriteriaDecision.decide(
                  matchingEnabled: manager.isDisplayCriteriaMatchingEnabled,
                  itemIsCurrent: itemIsCurrent,
                  openIsCurrent: openIsCurrent,
                  itemIsReady: item.status == .readyToPlay
              ) == .apply
        else { return false }
        manager.preferredDisplayCriteria = item.asset.preferredDisplayCriteria
        return true
    }

}
#endif

enum AudioInterruptionResponse: Equatable, Sendable {
    case suspend
    case resume
    case stay
    /// A `.began` that holds nothing: the viewer had already paused, so no
    /// system took the transport away from them.
    case ignore
}

@MainActor
final class PlaybackAudioSessionObserver {
    enum Event: Sendable {
        case interruption(AudioInterruptionResponse)
        case routeChange(revokesIntent: Bool)
    }

    nonisolated static func interruptionResponse(
        type: AVAudioSession.InterruptionType,
        options: AVAudioSession.InterruptionOptions,
        wantsPlayback: Bool
    ) -> AudioInterruptionResponse {
        switch type {
        case .began:
            // A viewer who already paused was not paused by the system. A hold
            // raised here could only be cleared by an `.ended`, and iOS does not
            // promise one for every `.began` — on 2026-10-04 an iPad paused for
            // ten minutes received a `.began`, never an `.ended`, and kept
            // "Paused — audio interrupted" over the picture for an hour.
            return wantsPlayback ? .suspend : .ignore
        case .ended:
            return options.contains(.shouldResume) && wantsPlayback ? .resume : .stay
        @unknown default:
            return .stay
        }
    }

    nonisolated static func routeChangeRevokesIntent(
        reason: AVAudioSession.RouteChangeReason
    ) -> Bool {
        reason == .oldDeviceUnavailable
    }

    private var tokens: [NSObjectProtocol] = []

    func start(
        wantsPlayback: @escaping @MainActor () -> Bool,
        receive: @escaping @MainActor (Event) -> Void
    ) {
        stop()
        let center = NotificationCenter.default
        let session = AVAudioSession.sharedInstance()
        tokens.append(center.addObserver(
            forName: AVAudioSession.interruptionNotification,
            object: session,
            queue: .main
        ) { notification in
            MainActor.assumeIsolated {
                guard let raw = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt,
                      let type = AVAudioSession.InterruptionType(rawValue: raw)
                else { return }
                let optionsRaw = notification.userInfo?[AVAudioSessionInterruptionOptionKey] as? UInt ?? 0
                receive(.interruption(Self.interruptionResponse(
                    type: type,
                    options: AVAudioSession.InterruptionOptions(rawValue: optionsRaw),
                    wantsPlayback: wantsPlayback()
                )))
            }
        })
        tokens.append(center.addObserver(
            forName: AVAudioSession.routeChangeNotification,
            object: session,
            queue: .main
        ) { notification in
            MainActor.assumeIsolated {
                guard let raw = notification.userInfo?[AVAudioSessionRouteChangeReasonKey] as? UInt,
                      let reason = AVAudioSession.RouteChangeReason(rawValue: raw)
                else { return }
                receive(.routeChange(revokesIntent: Self.routeChangeRevokesIntent(reason: reason)))
            }
        })
    }

    func stop() {
        for token in tokens { NotificationCenter.default.removeObserver(token) }
        tokens.removeAll()
    }

    deinit {
        for token in tokens { NotificationCenter.default.removeObserver(token) }
    }
}

/// The app's one holder of `UIApplication.isIdleTimerDisabled`, which is what
/// keeps the tvOS screensaver and iOS auto-lock off.
///
/// Every player stack used to leave display wake to AVPlayer's implicit
/// `preventsDisplaySleepDuringVideoPlayback`: a heuristic private to the
/// AVPlayer that decides for itself whether "video is playing" from its
/// binding to a visible layer, which the app can neither observe nor
/// reassert. On tvOS 26 it is reported to lapse once that binding changes and
/// not return until the item is replaced. Live TV moves its one long-lived
/// AVPlayer between the guide's picture and the fullscreen surface without
/// replacing the item, and the screensaver came on over live television in
/// both places (Apple TV, 2026-10-04). Android has always owned this
/// explicitly (`keepScreenOn`); this is the Apple counterpart. More than one
/// stack may hold at once, and the timer stays disabled while any of them does.
///
/// Every change is written through rather than only its edges: UIKit is not
/// promised to leave the flag alone, and a cached "already applied" would
/// leave a reset flag reset until every holder let go and claimed again.
@MainActor
final class DisplayWakeOwner {
    static let shared = DisplayWakeOwner { UIApplication.shared.isIdleTimerDisabled = $0 }

    private var holders: Set<UUID> = []
    private let apply: @MainActor (Bool) -> Void

    init(apply: @escaping @MainActor (Bool) -> Void) {
        self.apply = apply
    }

    var isHeld: Bool { !holders.isEmpty }

    func set(_ token: UUID, holding: Bool) {
        if holding { holders.insert(token) } else { holders.remove(token) }
        apply(!holders.isEmpty)
    }
}

/// One player stack's claim on display wake, derived from its AVPlayer the
/// way Android's `PlayerScreenOn` derives `keepScreenOn` from ExoPlayer: held
/// while the player is playing or waiting to play (the viewer asked for
/// playback and it is buffering), released as soon as it is paused (by the
/// viewer, by the system, by a failure or by a stop), never held for an
/// audio-only title, and never held while the picture is on an AirPlay
/// receiver rather than this screen. The claim follows the player rather than
/// any surface, so moving the picture between surfaces cannot drop it.
///
/// A stack's teardown must pause before it empties the player: an item-less
/// AVPlayer left at rate 1 reads as waiting to play, and holds.
@MainActor
final class PlaybackDisplayWake {
    nonisolated static func holds(
        status: AVPlayer.TimeControlStatus,
        hasVideo: Bool,
        external: Bool
    ) -> Bool {
        hasVideo && !external && status != .paused
    }

    private let owner: DisplayWakeOwner
    private let token = UUID()
    private weak var player: AVPlayer?
    private var observations: [NSKeyValueObservation] = []
    private var hasVideo = false

    init(owner: DisplayWakeOwner? = nil) {
        self.owner = owner ?? .shared
    }

    /// Idempotent: the same player keeps its observation, and a different
    /// one replaces it.
    func track(_ player: AVPlayer, hasVideo: Bool) {
        self.hasVideo = hasVideo
        if self.player !== player {
            observations.forEach { $0.invalidate() }
            self.player = player
            observations = [
                player.observe(\.timeControlStatus, options: [.new]) { [weak self] _, _ in
                    Task { @MainActor [weak self] in self?.sync() }
                },
                player.observe(\.isExternalPlaybackActive, options: [.new]) { [weak self] _, _ in
                    Task { @MainActor [weak self] in self?.sync() }
                },
            ]
        }
        sync()
    }

    func release() {
        observations.forEach { $0.invalidate() }
        observations = []
        player = nil
        owner.set(token, holding: false)
    }

    private func sync() {
        guard let player else {
            owner.set(token, holding: false)
            return
        }
        owner.set(token, holding: Self.holds(
            status: player.timeControlStatus,
            hasVideo: hasVideo,
            external: player.isExternalPlaybackActive
        ))
    }

    deinit {
        let owner = owner
        let token = token
        Task { @MainActor in owner.set(token, holding: false) }
    }
}
