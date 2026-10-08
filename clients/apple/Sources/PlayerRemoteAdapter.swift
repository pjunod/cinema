import SwiftUI

#if os(tvOS)
/// The sole tvOS event adapter for the player. Move commands are installed only
/// on the hidden surface and timeline so transport directions remain owned by
/// SwiftUI's focus engine.
struct PlayerRemoteAdapter: ViewModifier {
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    @EnvironmentObject private var playback: RemotePlaybackAdapter
    enum Scope {
        case root
        case surface
        case timeline
    }

    let scope: Scope
    let state: () -> PlayerInputState
    let apply: (PlayerInputOutcome, PlayerContractInput) -> Bool

    @ViewBuilder
    func body(content: Content) -> some View {
        switch scope {
        case .root:
            content
                .onExitCommand { dispatch(.back) }
                .onPlayPauseCommand { dispatch(.playPause) }
        case .surface, .timeline:
            content
                .onTapGesture { dispatch(.select) }
                .onMoveCommand { direction in
                    guard let input = Self.input(for: direction) else { return }
                    dispatch(input)
                }
        }
    }

    static func input(for direction: MoveCommandDirection) -> PlayerContractInput? {
        switch direction {
        case .left: .left
        case .right: .right
        case .up: .up
        case .down: .down
        @unknown default: nil
        }
    }

    private func dispatch(_ input: PlayerContractInput) {
        playback.physicalInput()
        navigation.physicalInput()
        _ = apply(
            PlayerInputRouting.route(surface: .tenFoot, state: state(), input: input),
            input
        )
    }
}

/// The same adapter for the live surface. It is here rather than in
/// `LiveTvView.swift` because this file is the platform's one allowed home for
/// remote decoding — `scripts/player-input-fence` enforces that, and a second
/// place that turns a `MoveCommandDirection` into an action is a second answer
/// to what the remote does. Live TV routes through its own table because a
/// live stream has no timeline to scrub.
struct LiveTvRemoteAdapter: ViewModifier {
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    @EnvironmentObject private var playback: RemotePlaybackAdapter
    enum Scope {
        case root
        case revealSurface
        /// A region whose directions are moved by its own navigator: the
        /// guide grid, and the page's detail region (picture + programme
        /// actions). `onMoveCommand` takes every direction pressed inside it,
        /// so every focusable in such a region must have a key the navigator
        /// can move to — one without is a trap.
        case guide
    }

    let scope: Scope
    let state: () -> LiveTvInputState
    let apply: (LiveTvInputOutcome, LiveTvContractInput) -> Bool

    @ViewBuilder
    func body(content: Content) -> some View {
        switch scope {
        case .root:
            content
                .onExitCommand { dispatch(.back) }
                .onPlayPauseCommand { dispatch(.playPause) }
        case .revealSurface:
            content
                .onTapGesture { dispatch(.select) }
                .onMoveCommand { direction in
                    guard let input = Self.input(for: direction) else { return }
                    dispatch(input)
                }
        case .guide:
            content.onMoveCommand { direction in
                guard let input = Self.input(for: direction) else { return }
                dispatch(input)
            }
        }
    }

    static func input(for direction: MoveCommandDirection) -> LiveTvContractInput? {
        switch direction {
        case .left: .left
        case .right: .right
        case .up: .up
        case .down: .down
        @unknown default: nil
        }
    }

    private func dispatch(_ input: LiveTvContractInput) {
        playback.physicalInput()
        navigation.physicalInput()
        _ = apply(
            LiveTvInputRouting.route(surface: .tenFoot, state: state(), input: input),
            input
        )
    }
}

extension View {
    func liveTvRemoteAdapter(
        _ scope: LiveTvRemoteAdapter.Scope,
        state: @escaping () -> LiveTvInputState,
        apply: @escaping (LiveTvInputOutcome, LiveTvContractInput) -> Bool
    ) -> some View {
        modifier(LiveTvRemoteAdapter(scope: scope, state: state, apply: apply))
    }

    func playerRemoteAdapter(
        _ scope: PlayerRemoteAdapter.Scope,
        state: @escaping () -> PlayerInputState,
        apply: @escaping (PlayerInputOutcome, PlayerContractInput) -> Bool
    ) -> some View {
        modifier(PlayerRemoteAdapter(scope: scope, state: state, apply: apply))
    }
}
#endif

#if os(tvOS)
/// Owned browsing modal Exit uses the same physical-adapter home as playback.
struct RemoteChoiceExitAdapter: ViewModifier {
    let navigation: RemoteNavigationCoordinator
    func body(content: Content) -> some View {
        content.onExitCommand {
            navigation.physicalInput()
            navigation.closeModal()
        }
    }
}
#endif

#if os(tvOS)
import GameController

/// Observe hardware value changes without replacing focus-engine direction
/// handling. This catches a Siri Remote press at a stationary focus edge.
/// Forward the existing handler and restore it when observation is stopped.
@MainActor
final class RemotePhysicalInputObserver: ObservableObject {
    private struct Subscription {
        let controller: GCController
        let handler: ((GCPhysicalInputProfile, GCControllerElement) -> Void)?
        let queue: DispatchQueue
    }
    private var subscriptions: [Subscription] = []
    private var connection: NSObjectProtocol?
    private var receive: () -> Void = {}
    func start(receive: @escaping () -> Void) {
        stop()
        self.receive = receive
        GCController.controllers().forEach(observe)
        connection = NotificationCenter.default.addObserver(forName: .GCControllerDidConnect, object: nil, queue: .main) { [weak self] note in
            guard let controller = note.object as? GCController else { return }
            MainActor.assumeIsolated { self?.observe(controller) }
        }
    }
    private func observe(_ controller: GCController) {
        guard !subscriptions.contains(where: { $0.controller === controller }) else { return }
        let previous = controller.physicalInputProfile.valueDidChangeHandler
        subscriptions.append(.init(controller: controller, handler: previous, queue: controller.handlerQueue))
        controller.handlerQueue = .main
        controller.physicalInputProfile.valueDidChangeHandler = { [weak self] profile, element in
            MainActor.assumeIsolated { self?.receive() }
            previous?(profile, element)
        }
    }
    func stop() {
        if let connection { NotificationCenter.default.removeObserver(connection) }
        connection = nil
        for subscription in subscriptions {
            subscription.controller.physicalInputProfile.valueDidChangeHandler = subscription.handler
            subscription.controller.handlerQueue = subscription.queue
        }
        subscriptions.removeAll()
        receive = {}
    }
}
#endif
