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

extension View {
    func playerRemoteAdapter(
        _ scope: PlayerRemoteAdapter.Scope,
        state: @escaping () -> PlayerInputState,
        apply: @escaping (PlayerInputOutcome, PlayerContractInput) -> Bool
    ) -> some View {
        modifier(PlayerRemoteAdapter(scope: scope, state: state, apply: apply))
    }
}
#endif
