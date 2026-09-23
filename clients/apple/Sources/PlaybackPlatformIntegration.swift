import AVFoundation
import AVKit
import Foundation
#if os(tvOS)
import UIKit
#endif

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
            return .suspend
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
