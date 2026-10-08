import AVKit
import SwiftUI
import UIKit

enum PictureInPictureCommand: Equatable {
    case start
    case stop
    case unavailable
}

struct PictureInPictureControlState: Equatable {
    let isButtonEnabled: Bool
    let command: PictureInPictureCommand
    let messageOnTap: String?
}

/// Owns the system Picture in Picture controller while PlayerView continues to
/// own the transport controls. Using the existing AVPlayerLayer avoids
/// reintroducing AVPlayerViewController's LIVE treatment for growing HLS
/// playlists.
@MainActor
final class PictureInPictureController: NSObject, ObservableObject,
                                        @preconcurrency AVPictureInPictureControllerDelegate {
    #if os(iOS)
    @Published private(set) var isSupported = AVPictureInPictureController.isPictureInPictureSupported()
    #else
    @Published private(set) var isSupported = false
    #endif
    @Published private(set) var isPossible = false
    @Published private(set) var isActive = false
    /// True from `willStart` until the start resolves either way.
    ///
    /// Automatic PiP begins as the app backgrounds, and iOS publishes
    /// `.inactive` before `.background`, so a caller that asks only `isActive`
    /// at the `.inactive` edge gets `false` for a window in which PiP is
    /// genuinely on its way. For Live TV that window was long enough to
    /// release the tuner before the PiP window it was starting for could
    /// appear — killing the one case automatic PiP exists for.
    @Published private(set) var isStarting = false
    @Published private(set) var errorMessage: String?

    private weak var playerLayer: AVPlayerLayer?
    private var controller: AVPictureInPictureController?
    private var possibleObservation: NSKeyValueObservation?
    private var activeObservation: NSKeyValueObservation?
    private var unavailableMessageTask: Task<Void, Never>?

    nonisolated static func command(isActive: Bool, isPossible: Bool) -> PictureInPictureCommand {
        if isActive { return .stop }
        return isPossible ? .start : .unavailable
    }

    /// The rendered control must remain reachable whenever this device supports
    /// PiP. Availability is an outcome of a tap, not permission to disable the
    /// only path that can explain why AVKit cannot start yet. Requiring the
    /// backing controller here also turns a stale published `isPossible` value
    /// after surface teardown into a visible unavailable result instead of a
    /// start message sent through a nil optional.
    nonisolated static func controlState(
        isSupported: Bool,
        isActive: Bool,
        isPossible: Bool,
        hasAttachedController: Bool
    ) -> PictureInPictureControlState {
        guard isSupported else {
            return PictureInPictureControlState(
                isButtonEnabled: false,
                command: .unavailable,
                messageOnTap: nil
            )
        }
        guard hasAttachedController else {
            return PictureInPictureControlState(
                isButtonEnabled: true,
                command: .unavailable,
                messageOnTap: "Picture in Picture isn't ready yet."
            )
        }
        let command = command(isActive: isActive, isPossible: isPossible)
        return PictureInPictureControlState(
            isButtonEnabled: true,
            command: command,
            messageOnTap: command == .unavailable
                ? "Picture in Picture isn't ready yet."
                : nil
        )
    }

    var controlState: PictureInPictureControlState {
        Self.controlState(
            isSupported: isSupported,
            isActive: isActive,
            isPossible: isPossible,
            hasAttachedController: controller != nil
        )
    }

    func attach(to playerLayer: AVPlayerLayer) {
        #if os(iOS)
        guard self.playerLayer !== playerLayer || controller == nil else { return }
        detach()
        self.playerLayer = playerLayer
        isSupported = AVPictureInPictureController.isPictureInPictureSupported()
        guard isSupported else { return }

        let source = AVPictureInPictureController.ContentSource(playerLayer: playerLayer)
        let controller = AVPictureInPictureController(contentSource: source)
        controller.delegate = self
        #if os(iOS)
        controller.canStartPictureInPictureAutomaticallyFromInline = true
        #endif
        self.controller = controller

        possibleObservation = controller.observe(\.isPictureInPicturePossible,
                                                 options: [.initial, .new]) { [weak self] controller, _ in
            Task { @MainActor in
                guard let self, self.controller === controller else { return }
                self.isPossible = controller.isPictureInPicturePossible
                if self.isPossible { self.clearErrorMessage() }
            }
        }
        activeObservation = controller.observe(\.isPictureInPictureActive,
                                               options: [.initial, .new]) { [weak self] controller, _ in
            Task { @MainActor in
                guard let self, self.controller === controller else { return }
                self.isActive = controller.isPictureInPictureActive
            }
        }
        #endif
    }

    func toggle() {
        clearErrorMessage()
        let state = controlState
        switch state.command {
        case .start:
            guard let controller else {
                showUnavailableMessage("Picture in Picture isn't ready yet.")
                return
            }
            controller.startPictureInPicture()
        case .stop:
            guard let controller else {
                showUnavailableMessage("Picture in Picture isn't ready yet.")
                return
            }
            controller.stopPictureInPicture()
        case .unavailable:
            showUnavailableMessage(state.messageOnTap)
        }
    }

    /// A tap while AVKit is not ready is useful feedback, not a playback
    /// failure. Give it the same bounded lifetime as the other nonfatal player
    /// notices so one retry cannot pin the banner and iOS system chrome.
    func showUnavailableMessage(_ message: String?, duration: Duration = .seconds(5)) {
        unavailableMessageTask?.cancel()
        unavailableMessageTask = nil
        errorMessage = message
        guard let message else { return }

        unavailableMessageTask = Task { [weak self] in
            do {
                try await Task.sleep(for: duration)
            } catch {
                return
            }
            guard let self, self.errorMessage == message else { return }
            self.errorMessage = nil
            self.unavailableMessageTask = nil
        }
    }

    /// AVKit's failure callback describes an attempted start, so unlike a
    /// not-yet-ready tap it remains until the player state changes.
    func showPersistentErrorMessage(_ message: String) {
        unavailableMessageTask?.cancel()
        unavailableMessageTask = nil
        errorMessage = message
    }

    private func clearErrorMessage() {
        unavailableMessageTask?.cancel()
        unavailableMessageTask = nil
        errorMessage = nil
    }

    private func stop() {
        if controller?.isPictureInPictureActive == true {
            controller?.stopPictureInPicture()
        }
    }

    func detach(resetPublishedState: Bool = true) {
        stop()
        possibleObservation = nil
        activeObservation = nil
        controller?.delegate = nil
        controller = nil
        playerLayer = nil
        if isStarting { isStarting = false }
        if resetPublishedState {
            if isPossible { isPossible = false }
            if isActive { isActive = false }
        }
    }

    func pictureInPictureControllerWillStartPictureInPicture(
        _ pictureInPictureController: AVPictureInPictureController
    ) {
        isStarting = true
    }

    func pictureInPictureControllerDidStartPictureInPicture(
        _ pictureInPictureController: AVPictureInPictureController
    ) {
        isStarting = false
        isActive = true
        clearErrorMessage()
    }

    func pictureInPictureControllerDidStopPictureInPicture(
        _ pictureInPictureController: AVPictureInPictureController
    ) {
        isStarting = false
        isActive = false
    }

    func pictureInPictureController(
        _ pictureInPictureController: AVPictureInPictureController,
        failedToStartPictureInPictureWithError error: Error
    ) {
        isStarting = false
        isActive = false
        showPersistentErrorMessage(
            "Picture in Picture couldn't start: \(error.localizedDescription)"
        )
    }

    func pictureInPictureController(
        _ pictureInPictureController: AVPictureInPictureController,
        restoreUserInterfaceForPictureInPictureStopWithCompletionHandler completionHandler: @escaping (Bool) -> Void
    ) {
        completionHandler(true)
    }
}

/// A video-only AVPlayer surface. Unlike SwiftUI's `VideoPlayer`, this view
/// does not install a second transport overlay or participate in the tvOS
/// focus engine; PlayerView remains the sole owner of playback controls.
struct PlayerSurface: UIViewRepresentable {
    let player: AVPlayer
    var stagedPlayer: AVPlayer? = nil
    var surfaceChanged: ((PlayerSurfaceView, Bool) -> Void)? = nil
    let pictureInPicture: PictureInPictureController
    let pgsOverlay: PGSOverlayWindow?
    let allowsPictureInPicture: Bool
    var presentationTargetChanged: ((Int?, Int?) -> Void)? = nil

    nonisolated static func shouldAllowPictureInPicture(
        isTearingDown: Bool,
        pgsOverlayIsActive: Bool
    ) -> Bool {
        !isTearingDown && !pgsOverlayIsActive
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(pictureInPicture: pictureInPicture)
    }

    func makeUIView(context: Context) -> PlayerSurfaceView {
        let view = PlayerSurfaceView()
        view.presentationTargetChanged = presentationTargetChanged
        view.pictureInPicture = pictureInPicture
        view.playerLayer.player = player
        view.stage(stagedPlayer)
        context.coordinator.surfaceChanged = surfaceChanged
        surfaceChanged?(view, true)
        view.applyPGSOverlay(pgsOverlay, to: player.currentItem)
        if allowsPictureInPicture {
            context.coordinator.pictureInPicture.attach(to: view.playerLayer)
        }
        return view
    }

    func updateUIView(_ view: PlayerSurfaceView, context: Context) {
        view.presentationTargetChanged = presentationTargetChanged
        view.reportPresentationTarget()
        if view.playerLayer.player !== player {
            view.playerLayer.player = player
        }
        view.stage(stagedPlayer)
        context.coordinator.surfaceChanged = surfaceChanged
        surfaceChanged?(view, true)
        view.applyPGSOverlay(pgsOverlay, to: player.currentItem)
        if allowsPictureInPicture {
            context.coordinator.pictureInPicture.attach(to: view.playerLayer)
        } else {
            context.coordinator.pictureInPicture.detach()
        }
    }

    static func dismantleUIView(_ view: PlayerSurfaceView, coordinator: Coordinator) {
        // SwiftUI is invalidating its observation graph while this callback
        // runs. Publishing from here violates Swift's exclusivity rules on
        // tvOS, so tear down the AVKit objects without notifying a view that
        // is already being destroyed. A surviving controller is reset by the
        // normal detach at the start of its next attachment.
        coordinator.pictureInPicture.detach(resetPublishedState: false)
        view.applyPGSOverlay(nil, to: nil)
        view.playerLayer.player = nil
        view.stage(nil)
        coordinator.surfaceChanged?(view, false)
        coordinator.surfaceChanged = nil
        view.presentationTargetChanged?(nil, nil)
        view.presentationTargetChanged = nil
        #if os(tvOS)
        PlaybackDisplayCriteria.activeManager()?.preferredDisplayCriteria = nil
        #endif
    }

    final class Coordinator {
        let pictureInPicture: PictureInPictureController
        var surfaceChanged: ((PlayerSurfaceView, Bool) -> Void)?

        init(pictureInPicture: PictureInPictureController) {
            self.pictureInPicture = pictureInPicture
        }
    }
}

/// Values read together on the main actor from the active (never staged) layer.
struct LocalVideoEvidence: Equatable {
    var bindingMatches: Bool
    var targetVisible: Bool
    var readyForDisplay: Bool

    static let unavailable = LocalVideoEvidence(
        bindingMatches: false, targetVisible: false, readyForDisplay: false)
    var frameReady: Bool { bindingMatches && targetVisible && readyForDisplay }
}

final class PlayerSurfaceView: UIView {
    private(set) var playerLayer = AVPlayerLayer()
    private var stagedLayer = AVPlayerLayer()
    weak var pictureInPicture: PictureInPictureController?
    var presentationTargetChanged: ((Int?, Int?) -> Void)?

    /// Both layers stay attached. Promotion changes visibility without moving
    /// an item or rebinding the target's already prepared display layer.
    func stage(_ player: AVPlayer?) {
        guard stagedLayer.player !== player else { return }
        stagedLayer.player = player
    }

    func canPromote(_ player: AVPlayer) -> Bool {
        window?.windowScene?.activationState == .foregroundActive
            && pictureInPicture?.isActive != true && pictureInPicture?.isStarting != true
            && stagedLayer.player === player && stagedLayer.isReadyForDisplay
    }

    @discardableResult
    func promote(_ player: AVPlayer) -> Bool {
        guard canPromote(player) else { return false }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        let previous = playerLayer
        playerLayer = stagedLayer
        stagedLayer = previous
        playerLayer.opacity = 1
        stagedLayer.opacity = 0
        observeVideoRect()
        setNeedsLayout()
        CATransaction.commit()
        return true
    }

    private func observeVideoRect() {
        videoRectObservation = playerLayer.observe(\.videoRect, options: [.new]) {
            [weak self] _, _ in
            Task { @MainActor in self?.setNeedsLayout() }
        }
    }

    func videoEvidence(for expectedPlayer: AVPlayer, item expectedItem: AVPlayerItem) -> LocalVideoEvidence {
        let matches = playerLayer.player === expectedPlayer && expectedPlayer.currentItem === expectedItem
        return LocalVideoEvidence(
            bindingMatches: matches,
            targetVisible: visibleTargetSize != nil,
            readyForDisplay: matches && expectedItem.status == .readyToPlay && playerLayer.isReadyForDisplay
        )
    }

    private var visibleTargetSize: CGSize? {
        var ancestor: UIView? = self
        while let view = ancestor {
            guard !view.isHidden, view.alpha > 0 else { return nil }
            ancestor = view.superview
        }
        guard let window, window.windowScene?.activationState == .foregroundActive,
              !playerLayer.isHidden, playerLayer.opacity > 0,
              Self.finiteNonempty(playerLayer.frame), Self.finiteNonempty(bounds),
              Self.finiteNonempty(convert(bounds, to: window).intersection(window.bounds)),
              Self.finiteNonempty(playerLayer.frame.intersection(bounds)) else { return nil }
        let size = CGSize(width: bounds.width * window.screen.scale, height: bounds.height * window.screen.scale)
        guard size.width >= 1, size.height >= 1, size.width <= 16384, size.height <= 16384 else { return nil }
        return size
    }

    private static func finiteNonempty(_ rect: CGRect) -> Bool {
        !rect.isEmpty && !rect.isNull && rect.origin.x.isFinite && rect.origin.y.isFinite
            && rect.width.isFinite && rect.height.isFinite
    }

    func reportPresentationTarget() {
        guard let size = visibleTargetSize else {
            presentationTargetChanged?(nil, nil)
            return
        }
        presentationTargetChanged?(Int(size.width.rounded()), Int(size.height.rounded()))
    }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        reportPresentationTarget()
    }

    private struct OverlayNode {
        let layer: CALayer
        let object: PGSOverlayObject
        let canvasWidth: Int
        let canvasHeight: Int
    }

    private var synchronizedLayer: AVSynchronizedLayer?
    private var overlayNodes: [OverlayNode] = []
    private var overlayRevision: Int?
    private weak var overlayItem: AVPlayerItem?
    private var videoRectObservation: NSKeyValueObservation?

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .black
        isUserInteractionEnabled = false
        isAccessibilityElement = false
        playerLayer.videoGravity = .resizeAspect
        stagedLayer.videoGravity = .resizeAspect
        stagedLayer.opacity = 0
        layer.addSublayer(playerLayer)
        layer.addSublayer(stagedLayer)
        clipsToBounds = true
        observeVideoRect()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is unavailable")
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        playerLayer.frame = bounds
        stagedLayer.frame = bounds
        reportPresentationTarget()
        let videoRect = playerLayer.videoRect
        synchronizedLayer?.frame = videoRect
        let destination = CGRect(origin: .zero, size: videoRect.size)
        for node in overlayNodes {
            node.layer.frame = PGSOverlayPolicy.objectFrame(
                node.object,
                canvasWidth: node.canvasWidth,
                canvasHeight: node.canvasHeight,
                destination: destination
            )
        }
        CATransaction.commit()
    }

    func applyPGSOverlay(_ window: PGSOverlayWindow?, to item: AVPlayerItem?) {
        guard let window, let item else {
            detachPGSOverlay()
            return
        }
        guard overlayRevision != window.revision || overlayItem !== item else {
            setNeedsLayout()
            return
        }

        detachPGSOverlay()
        overlayRevision = window.revision
        overlayItem = item

        let synchronized = AVSynchronizedLayer(playerItem: item)
        synchronized.masksToBounds = true
        synchronizedLayer = synchronized
        layer.addSublayer(synchronized)

        for renderable in window.cues {
            let cue = renderable.cue
            guard let interval = PGSOverlayPolicy.itemInterval(
                cue: cue,
                baseMs: window.baseMs
            ) else { continue }
            let itemStart = interval.lowerBound
            let itemEnd = interval.upperBound

            for object in renderable.objects {
                let imageLayer = CALayer()
                imageLayer.contents = object.image
                imageLayer.contentsGravity = .resize
                imageLayer.opacity = 0
                imageLayer.actions = [
                    "bounds": NSNull(),
                    "position": NSNull(),
                    "opacity": NSNull(),
                ]

                let visibility = CABasicAnimation(keyPath: "opacity")
                visibility.fromValue = 1
                visibility.toValue = 1
                visibility.beginTime = AVCoreAnimationBeginTimeAtZero
                    + Double(itemStart) / 1_000
                visibility.duration = Double(itemEnd - itemStart) / 1_000
                visibility.fillMode = .removed
                visibility.isRemovedOnCompletion = true
                imageLayer.add(visibility, forKey: "pgs-visibility")
                synchronized.addSublayer(imageLayer)
                overlayNodes.append(OverlayNode(
                    layer: imageLayer,
                    object: object.object,
                    canvasWidth: cue.canvasWidth,
                    canvasHeight: cue.canvasHeight
                ))
            }
        }
        setNeedsLayout()
    }

    private func detachPGSOverlay() {
        synchronizedLayer?.removeFromSuperlayer()
        synchronizedLayer = nil
        overlayNodes.removeAll(keepingCapacity: false)
        overlayRevision = nil
        overlayItem = nil
    }

    /// Test seam for the item-replacement invariant. The renderer must never
    /// leave the predecessor item's synchronized layer attached.
    func hasPGSOverlay(revision: Int, item: AVPlayerItem) -> Bool {
        overlayRevision == revision && overlayItem === item && synchronizedLayer != nil
    }

    var pgsOverlayNodeCount: Int { overlayNodes.count }

    deinit { videoRectObservation = nil }

    #if os(tvOS)
    override var canBecomeFocused: Bool { false }
    #endif
}
