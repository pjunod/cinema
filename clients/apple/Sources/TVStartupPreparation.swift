import SwiftUI
#if os(tvOS)
import UIKit
#endif

#if os(tvOS)
/// The player is presented as a full-screen cover in this same window.
struct TVFullscreenTargetProbe: UIViewRepresentable {
    let report: (PresentationTarget?) -> Void

    final class ProbeView: UIView {
        var report: ((PresentationTarget?) -> Void)?

        override func didMoveToWindow() {
            super.didMoveToWindow()
            let notifications = NotificationCenter.default
            notifications.removeObserver(self)
            if let window {
                for name in [UIWindow.didBecomeKeyNotification, UIWindow.didResignKeyNotification] {
                    notifications.addObserver(self, selector: #selector(windowStateChanged(_:)),
                                              name: name, object: window)
                }
                if let scene = window.windowScene {
                    for name in [UIScene.didActivateNotification, UIScene.willDeactivateNotification] {
                        notifications.addObserver(self, selector: #selector(windowStateChanged(_:)),
                                                  name: name, object: scene)
                    }
                }
            }
            measure()
        }

        @objc private func windowStateChanged(_ notification: Notification) {
            measure()
        }

        override func layoutSubviews() {
            super.layoutSubviews()
            measure()
        }

        func measure() {
            let target: PresentationTarget?
            if let window, window.isKeyWindow,
               window.windowScene?.activationState == .foregroundActive,
               window.bounds.size == window.screen.bounds.size,
               let size = backingPresentationSize(window.bounds.size, scale: window.screen.scale) {
                target = PresentationTarget(widthPx: Int(size.width.rounded()),
                    heightPx: Int(size.height.rounded()), revision: 1)
            } else {
                target = nil
            }
            // Layout can run during SwiftUI view updates.
            Task { @MainActor [weak self] in self?.report?(target) }
        }
    }

    func makeUIView(context: Context) -> ProbeView {
        let view = ProbeView()
        view.isUserInteractionEnabled = false
        return view
    }

    func updateUIView(_ view: ProbeView, context: Context) {
        view.report = report
        view.measure()
    }
}

/// Opportunistic server preparation of the exact resume window a Play will
/// request. Title detail and the home Continue Watching feature both offer it
/// while their Resume action is on screen; Play still requests a current
/// decision, so a missed or stale preparation only costs the cold start.
struct TVStartupPreparation: Equatable {
    let itemId: Int
    let fileId: Int
    let startMs: Int
    let selection: PrePlaySelection
    let quality: PlaybackQuality
    let authorizationGeneration: UInt64
    let presentationTarget: PresentationTarget?
    let routeV1: Bool
    let deviceSubtitlesOff: Bool
    let subtitleReadiness: SubtitleReadiness

    /// The same request Play will construct for `context`, or nil when the
    /// viewer, session or display cannot yet describe it exactly.
    @MainActor
    static func offer(
        for context: PlayContext, model: AppModel, presentationTarget: PresentationTarget?
    ) -> TVStartupPreparation? {
        let authorization = Session.shared.playbackAuthorization
        let quality = model.playbackQuality
        let target = model.displayAwareAuto ? presentationTarget : nil
        guard model.phase == .ready, authorization.token != nil,
              !model.displayAwareAuto || target != nil,
              context.fileId > 0, quality.rungHeight == nil else { return nil }
        return TVStartupPreparation(
            itemId: context.itemId, fileId: context.fileId, startMs: context.startMs,
            selection: context.selection, quality: quality,
            authorizationGeneration: authorization.generation,
            presentationTarget: target,
            routeV1: model.displayAwareAuto && model.displayAwareAutoProtocol == "route-v1",
            deviceSubtitlesOff: model.subLang == "off", subtitleReadiness: model.subtitleReadiness
        )
    }

    /// Ask the server to prepare this window. Failures are silent by design.
    @MainActor
    func run(using model: AppModel) async {
        guard !Task.isCancelled,
              Session.shared.playbackAuthorization.generation == authorizationGeneration else { return }
        do {
            let preview = try await Caps.PresentationContext.$target.withValue(presentationTarget) {
                try await model.playbackDecision(
                    fileId: fileId, selection: selection, quality: quality, speculative: true
                )
            }
            guard !Task.isCancelled,
                  Session.shared.playbackAuthorization.generation == authorizationGeneration,
                  let body = PlayerController.startupPreparationRequest(
                    decision: preview.decision, caps: preview.caps, selection: selection,
                    quality: quality, startMs: startMs,
                    deviceSubtitlesOff: deviceSubtitlesOff,
                    subtitleReadiness: subtitleReadiness, routeV1: routeV1,
                    playbackId: UUID().uuidString
                  ) else { return }
            try await model.prepareHlsStartup(fileId: fileId, body: body)
        } catch {
            // Preparation is opportunistic. Play still requests a current decision.
        }
    }
}
#endif
