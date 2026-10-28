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
        case .surface:
            content
                .onTapGesture { dispatch(.select) }
                .onMoveCommand { direction in
                    guard let input = Self.input(for: direction) else { return }
                    dispatch(input)
                }
        case .timeline:
            // A focusable drawing with a tap gesture can receive directions
            // while dropping Siri Remote Select. Use native button activation
            // and keep the label-only style so the thin bar gains no bezel.
            Button { dispatch(.select) } label: { content }
                .buttonStyle(PlaybackRevealButtonStyle())
                .focusEffectDisabled()
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
        /// A mounted transport row with explicit directional neighbours.
        case controls
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
        case .controls:
            content
                .onExitCommand { dispatch(.back) }
                .onPlayPauseCommand { dispatch(.playPause) }
                .onMoveCommand { direction in
                    guard let input = Self.input(for: direction) else { return }
                    dispatch(input)
                }
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
struct RemotePhysicalInputTransitions {
    private var active: Set<String> = []
    mutating func reset(to current: Set<String> = []) { active = current }
    mutating func observe(_ current: Set<String>) -> Bool {
        let activated = !current.subtracting(active).isEmpty
        active = current
        return activated
    }
}

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
        var transitions = RemotePhysicalInputTransitions()
        transitions.reset(to: Self.activeInputs(controller.physicalInputProfile))
        subscriptions.append(.init(controller: controller, handler: previous, queue: controller.handlerQueue))
        controller.handlerQueue = .main
        controller.physicalInputProfile.valueDidChangeHandler = { [weak self] profile, element in
            MainActor.assumeIsolated {
                if transitions.observe(Self.activeInputs(profile)) { self?.receive() }
            }
            previous?(profile, element)
        }
    }
    private static func activeInputs(_ profile: GCPhysicalInputProfile) -> Set<String> {
        var active: Set<String> = []
        var directionalButtons: Set<ObjectIdentifier> = []
        var directionalAxes: Set<ObjectIdentifier> = []
        for (name, pad) in profile.dpads {
            [pad.up, pad.down, pad.left, pad.right].forEach { directionalButtons.insert(ObjectIdentifier($0)) }
            [pad.xAxis, pad.yAxis].forEach { directionalAxes.insert(ObjectIdentifier($0)) }
            if pad.xAxis.value > 0.5 { active.insert(name + ":right") }
            if pad.xAxis.value < -0.5 { active.insert(name + ":left") }
            if pad.yAxis.value > 0.5 { active.insert(name + ":up") }
            if pad.yAxis.value < -0.5 { active.insert(name + ":down") }
        }
        for (name, button) in profile.buttons where !directionalButtons.contains(ObjectIdentifier(button)) {
            if button.isPressed { active.insert("button:" + name) }
        }
        for (name, axis) in profile.axes where !directionalAxes.contains(ObjectIdentifier(axis)) {
            if axis.value > 0.5 { active.insert("axis:" + name + ":positive") }
            if axis.value < -0.5 { active.insert("axis:" + name + ":negative") }
        }
        return active
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

#if os(tvOS)
/// Only the settings content pane owns this Back. The category rail remains
/// outside the adapter, retaining native root behavior and spatial movement.
private struct TVSettingsContentExitAdapter: ViewModifier {
    let returnToCategory: () -> Void
    func body(content: Content) -> some View {
        content.onExitCommand(perform: returnToCategory)
    }
}
extension View {
    func tvSettingsContentExit(_ returnToCategory: @escaping () -> Void) -> some View {
        modifier(TVSettingsContentExitAdapter(returnToCategory: returnToCategory))
    }
}
#endif

extension View {
    @ViewBuilder
    func remotePhysicalDirections() -> some View {
        #if os(tvOS)
        modifier(RemotePhysicalDirectionIntentAdapter())
        #else
        self
        #endif
    }
}
#if os(tvOS)
/// Observe arrow presses, including simulator keyboard-style input, while
/// returning ignored so SwiftUI's spatial focus engine retains movement.
private struct RemotePhysicalDirectionIntentAdapter: ViewModifier {
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    func body(content: Content) -> some View {
        content.onKeyPress(keys: [.upArrow, .downArrow, .leftArrow, .rightArrow], phases: [.down, .repeat]) { _ in
            navigation.physicalInput()
            return .ignored
        }
    }
}
#endif

#if os(tvOS)
private struct TVDiagnosticReportExitAdapter: ViewModifier {
    let close: () -> Void
    func body(content: Content) -> some View { content.onExitCommand(perform: close) }
}
extension View {
    func tvDiagnosticReportExit(_ close: @escaping () -> Void) -> some View {
        modifier(TVDiagnosticReportExitAdapter(close: close))
    }

    /// The viewer navigation panel owns Menu only while its caller is expanded.
    func tvViewerNavigationPanelExit(_ close: @escaping () -> Void) -> some View {
        onExitCommand(perform: close)
    }
}
#endif

#if os(tvOS)
/// A hidden surface supplies bounded native focus neighbours. Moving to any
/// neighbour reveals controls; visible transport movement remains native.
struct LiveTvRevealSurface: View {
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    @EnvironmentObject private var playback: RemotePlaybackAdapter
    let state: () -> LiveTvInputState
    let apply: (LiveTvInputOutcome, LiveTvContractInput) -> Bool
    var identifier = "live-player-reveal"
    var body: some View {
        NativePlaybackRevealTarget(identifier: identifier, enabled: { state() == .fullscreenHidden },
                                   reveal: { dispatch(.select) }, direction: { direction in
            guard let input = LiveTvRemoteAdapter.input(for: direction) else { return }
            dispatch(input)
        })
    }
    private func dispatch(_ input: LiveTvContractInput) {
        guard state() == .fullscreenHidden else { return }
        playback.physicalInput(); navigation.physicalInput()
        _ = apply(LiveTvInputRouting.route(surface: .tenFoot, state: state(), input: input), input)
    }
}

struct PlayerRevealSurface: View {
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    @EnvironmentObject private var playback: RemotePlaybackAdapter
    let state: () -> PlayerInputState
    let apply: (PlayerInputOutcome, PlayerContractInput) -> Bool
    var identifier = "player-reveal"
    var body: some View {
        NativePlaybackRevealTarget(identifier: identifier, enabled: { state() == .hidden },
                                   reveal: { dispatch(.select) }, direction: { direction in
            guard let input = PlayerRemoteAdapter.input(for: direction) else { return }
            dispatch(input)
        })
    }
    private func dispatch(_ input: PlayerContractInput) {
        guard state() == .hidden else { return }
        playback.physicalInput(); navigation.physicalInput()
        _ = apply(PlayerInputRouting.route(surface: .tenFoot, state: state(), input: input), input)
    }
}

/// tvOS PlainButtonStyle still paints a focused bezel over a clear label.
/// Keep the native button/input semantics, but render only our clear label.
private struct PlaybackRevealButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View { configuration.label }
}

private struct NativePlaybackRevealTarget: View {
    let identifier: String
    let enabled: () -> Bool
    let reveal: () -> Void
    let direction: (MoveCommandDirection) -> Void
    @FocusState private var focused: Bool

    var body: some View {
        Button { if enabled() { reveal() } } label: {
            Color.clear.contentShape(Rectangle()).frame(width: 120, height: 120)
        }
        .buttonStyle(PlaybackRevealButtonStyle())
        .focusEffectDisabled()
        .focused($focused)
        .accessibilityLabel("Show playback controls")
        .accessibilityIdentifier(identifier)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .defaultFocus($focused, true)
        .onMoveCommand { if enabled() { direction($0) } }
        .task { focused = true }
    }
}
#endif

#if os(tvOS)
/// Focus state belongs to the presented controls, not the covered browser.
struct NativePlaybackFocusRegion<Key: Hashable, Content: View>: View {
    @FocusState private var focused: Key?
    let preferred: Key
    let requested: Key?
    let changed: () -> Void
    @ViewBuilder let content: (FocusState<Key?>.Binding) -> Content
    var body: some View {
        content($focused)
            // Request entry once. A defaultFocus preference that follows the
            // observed control is reapplied during directional movement and
            // can make one press skip the adjacent control.
            .onAppear { focused = preferred }
            .onChange(of: requested) { _, value in
                // Requests are presentation intents, never native-focus
                // observations echoed back by the parent.
                if focused != value { focused = value }
            }
            .onChange(of: focused) { _, _ in changed() }
    }
}
#endif

extension View {
    @ViewBuilder
    func playbackControlFocus(_ binding: FocusState<String?>.Binding?, equals key: String) -> some View {
        if let binding { focused(binding, equals: key) } else { self }
    }
}

#if os(tvOS)
/// Back from inside a long collection moves focus to its toolbar instead of
/// leaving the screen. Back from the toolbar is outside this adapter, so the
/// native pop still happens there.
private struct RemoteContentBackAdapter: ViewModifier {
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    @Environment(\.remoteNavigationScope) private var scope
    let target: String
    let reveal: () -> Void
    func body(content: Content) -> some View {
        content.onExitCommand {
            navigation.physicalInput()
            reveal()
            navigation.focusControl(scope: scope, key: target)
        }
    }
}
/// After Back, the toolbar returns Down to the title Back was pressed on,
/// whatever column sits beneath the focused tool. Other directions, and Down
/// when no return is pending, stay native.
private struct RemoteContentReturnAdapter: ViewModifier {
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    @Environment(\.remoteNavigationScope) private var scope
    let target: String?
    let consumed: () -> Void
    func body(content: Content) -> some View {
        content.onMoveCommand { direction in
            guard direction == .down, let target else { return }
            navigation.physicalInput()
            navigation.focusControl(scope: scope, key: target)
            consumed()
        }
    }
}
extension View {
    func remoteContentReturn(to target: String?, consumed: @escaping () -> Void = {}) -> some View {
        modifier(RemoteContentReturnAdapter(target: target, consumed: consumed))
    }
    func remoteContentBack(to target: String, reveal: @escaping () -> Void = {}) -> some View {
        modifier(RemoteContentBackAdapter(target: target, reveal: reveal))
    }
}
#endif

/// A countdown never authorizes hiding from the state it saw before waiting.
/// Presentation ownership, pause and failure may all change during the delay.
@MainActor
enum PlaybackControlAutoHide {
    static func afterDelay(
        while eligible: () -> Bool,
        delay: () async throws -> Void = {
            try await Task.sleep(nanoseconds: LiveTvInputRouting.overlayAutoHideNanoseconds)
        }
    ) async -> Bool {
        guard eligible() else { return false }
        do { try await delay() } catch { return false }
        return !Task.isCancelled && eligible()
    }
}
