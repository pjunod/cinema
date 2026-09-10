import SwiftUI

#if os(tvOS)
/// The sole tvOS event adapter for the player. Move commands are installed only
/// on the hidden surface and timeline so transport directions remain owned by
/// SwiftUI's focus engine.
struct PlayerRemoteAdapter: ViewModifier {
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
    enum Scope {
        case root
        case revealSurface
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
