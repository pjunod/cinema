import SwiftUI
import UIKit

private struct RemoteScopeKey: EnvironmentKey { static let defaultValue = "restricted" }
extension EnvironmentValues {
    var remoteNavigationScope: String {
        get { self[RemoteScopeKey.self] }
        set { self[RemoteScopeKey.self] = newValue }
    }
}

private struct RemoteNativeTarget: Equatable {
    let scope: String
    let key: String
}
private struct RemoteNativeTargetKey: EnvironmentKey {
    static let defaultValue: RemoteNativeTarget? = nil
}
private struct RemoteNativeObservation {
    let revision: String?
    let changed: (Bool) -> Void
}
private struct RemoteNativeObservationKey: EnvironmentKey {
    static let defaultValue: RemoteNativeObservation? = nil
}
private extension EnvironmentValues {
    var remoteNativeObservation: RemoteNativeObservation? {
        get { self[RemoteNativeObservationKey.self] }
        set { self[RemoteNativeObservationKey.self] = newValue }
    }
    var remoteNativeTarget: RemoteNativeTarget? {
        get { self[RemoteNativeTargetKey.self] }
        set { self[RemoteNativeTargetKey.self] = newValue }
    }
}

/// Installed inside a ButtonStyle, where isFocused is the native button's
/// environment. A background on the outside of Button cannot observe it.
private struct RemoteNativeFocusModifier: ViewModifier {
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    @Environment(\.remoteNativeTarget) private var target
    @Environment(\.remoteNativeObservation) private var observation
    let focused: Bool
    private var requestRevision: UInt64 { target == nil ? 0 : navigation.focusRequestRevision }
    func body(content: Content) -> some View {
        content.task(id: ReportID(focused: focused, requestRevision: requestRevision, observationRevision: observation?.revision)) {
            guard !Task.isCancelled else { return }
            report(focused)
        }
    }
    private struct ReportID: Hashable {
        let focused: Bool
        let requestRevision: UInt64
        let observationRevision: String?
    }
    private func report(_ value: Bool) {
        observation?.changed(value)
        guard let target else { return }
        navigation.nativeControlFocus(scope: target.scope, key: target.key, focused: value)
    }
}
extension View {
    /// Observe physical-only controls without registering any companion action.
    func remoteNativeFocusObservation(revision: String? = nil, _ changed: @escaping (Bool) -> Void) -> some View {
        environment(\.remoteNativeObservation, RemoteNativeObservation(revision: revision, changed: changed))
    }
    func remoteNativeFocus(_ actuallyFocused: Bool) -> some View {
        modifier(RemoteNativeFocusModifier(focused: actuallyFocused))
    }
}

/// A root-owned probe checks presentation synchronously at dispatch, including
/// a system keyboard/menu opened by local input. SwiftUI overlays also require
/// explicit restriction tokens; this probe is deliberately not the only fence.
struct RemotePresentationProbe: UIViewRepresentable {
    let navigation: RemoteNavigationCoordinator
    final class Probe: UIView {}
    func makeUIView(context: Context) -> Probe {
        let probe = Probe()
        navigation.presentationBlocked = { [weak probe] in
            guard let window = probe?.window, let root = window.rootViewController else { return true }
            @MainActor func presented(_ controller: UIViewController) -> Bool {
                if let next = controller.presentedViewController {
                    guard navigation.ownsPresentation(ObjectIdentifier(next)) else { return true }
                    if presented(next) { return true }
                }
                return controller.children.contains(where: presented)
            }
            return presented(root)
        }
        return probe
    }
    func updateUIView(_ uiView: Probe, context: Context) {}
}

private struct RemoteRestrictedModifier: ViewModifier {
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    @State private var token = UUID()
    let restricted: Bool
    func body(content: Content) -> some View {
        content
            .onAppear { navigation.setBlocked(token, restricted) }
            .onChange(of: restricted) { _, value in navigation.setBlocked(token, value) }
            .onDisappear { navigation.setBlocked(token, false) }
    }
}

private struct RemoteControlModifier: ViewModifier {
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    @Environment(\.remoteNavigationScope) private var scope
    @FocusState private var focused: Bool
    @State private var registration = UUID()
    @State private var frame = CGRect.zero
    let key: String
    let label: String
    let activate: () -> Void
    let enabled: Bool
    let managesFocus: Bool
    let observesPhysicalActivation: Bool
    let admission: () -> RemoteNavigationCoordinator.Outcome?
    func body(content: Content) -> some View {
        content
            .modifier(RemoteControlFocusBinding(focused: $focused, managesFocus: managesFocus))
            .simultaneousGesture(TapGesture().onEnded { if observesPhysicalActivation { navigation.physicalControlActivation(scope: scope) } })
            .onGeometryChange(for: CGRect.self) { $0.frame(in: .global) } action: { value in
                frame = value
                register()
            }
            .onChange(of: enabled) { _, _ in register() }
            .onAppear { register() }
            .task(id: navigation.focusRequestRevision) {
                guard managesFocus, navigation.requestedFocus == key, navigation.activeScope == scope else { return }
                let revision = navigation.focusRequestRevision
                // A request made under a native cover may leave the binding
                // true without granting focus. Dismissal must create an edge.
                focused = false
                await Task.yield()
                guard !Task.isCancelled, navigation.focusRequestRevision == revision,
                      navigation.requestedFocus == key, navigation.activeScope == scope else { return }
                focused = true
            }
            .background(RemoteActualFocusObserver(scope: scope, key: key).allowsHitTesting(false))
            .background(RemoteRegistrationUpdate(update: register).allowsHitTesting(false))
            .onDisappear { navigation.unregister(scope: scope, key: key, id: registration) }
            .environment(\.remoteNativeTarget, RemoteNativeTarget(scope: scope, key: key))
    }
    private func register() {
        guard enabled else { navigation.unregister(scope: scope, key: key, id: registration); return }
        navigation.register(scope: scope, key: key,
                            entry: .init(id: registration, label: label, frame: frame, admission: admission, activate: activate))
    }
}

private struct RemoteControlFocusBinding: ViewModifier {
    @FocusState.Binding var focused: Bool
    let managesFocus: Bool
    @ViewBuilder func body(content: Content) -> some View {
        if managesFocus { content.focused($focused) } else { content }
    }
}

/// SwiftUI can replace an action at identical geometry. Refresh the callback
/// on each native view update while retaining the same realization identity.
private struct RemoteRegistrationUpdate: UIViewRepresentable {
    let update: () -> Void
    func makeUIView(context: Context) -> UIView { let view = UIView(); update(); return view }
    func updateUIView(_ uiView: UIView, context: Context) { update() }
}

/// isFocused comes from the native focus environment, not the requested
/// FocusState binding. An unfulfilled request cannot authorize Select.
private struct RemoteActualFocusObserver: View {
    @Environment(\.isFocused) private var actuallyFocused
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    let scope: String
    let key: String
    var body: some View {
        Color.clear
            .task(id: actuallyFocused) { if actuallyFocused { navigation.physicalFocus(scope: scope, key: key) } }
            .onChange(of: actuallyFocused) { _, value in
                if value { navigation.physicalFocus(scope: scope, key: key) }
                else if navigation.focusedKey == key { navigation.physicalFocus(scope: scope, key: nil) }
            }
    }
}

private struct RemoteScopeLifecycle: ViewModifier {
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    let scope: String
    func body(content: Content) -> some View {
        content.onAppear { navigation.retainScope(scope) }
            .onDisappear { navigation.releaseScope(scope) }
            .remotePhysicalDirections()
    }
}

private struct RemoteScrollRealization: ViewModifier {
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    @Environment(\.remoteNavigationScope) private var scope
    func body(content: Content) -> some View {
        ScrollViewReader { proxy in
            content.onChange(of: navigation.focusRequestRevision) { _, _ in
                guard navigation.activeScope == scope, let key = navigation.requestedFocus else { return }
                proxy.scrollTo(key, anchor: .center)
            }
        }
    }
}

extension View {
    func remoteScrollRealization() -> some View { modifier(RemoteScrollRealization()) }
    func remoteScope(_ scope: String) -> some View {
        environment(\.remoteNavigationScope, scope).modifier(RemoteScopeLifecycle(scope: scope))
    }
    func remoteRestricted(_ restricted: Bool = true) -> some View {
        modifier(RemoteRestrictedModifier(restricted: restricted))
    }
    func remoteControl(_ key: String, label: String, enabled: Bool = true, managesFocus: Bool = true, observesPhysicalActivation: Bool = true, admission: @escaping () -> RemoteNavigationCoordinator.Outcome? = { nil }, activate: @escaping () -> Void) -> some View {
        modifier(RemoteControlModifier(key: key, label: label, activate: activate, enabled: enabled, managesFocus: managesFocus, observesPhysicalActivation: observesPhysicalActivation, admission: admission))
    }
}

struct RemoteChoice: Identifiable {
    let id: String
    let label: String
    let selected: Bool
    var returnFocus: String? = nil
    let choose: () -> Void
}

/// Content for the shared native presentation rather than a system Menu/Picker. Both input sources invoke
/// the same choice closures, and selected values remain accessible.
struct RemoteChoicePanel: View {
    @Namespace private var focusNamespace
    @FocusState private var nativeChoice: String?
    #if os(tvOS)
    @Environment(\.resetFocus) private var resetFocus
    #endif
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    let scope: String
    let title: String
    let choices: [RemoteChoice]
    var admission: () -> RemoteNavigationCoordinator.Outcome? = { nil }
    var body: some View {
        ScrollViewReader { _ in
        ScrollView {
        VStack(alignment: .leading, spacing: 18) {
            Text(title).font(.title2.bold()).accessibilityAddTraits(.isHeader)
            ForEach(choices) { choice in
                Button {
                    navigation.performPhysicalAction { commit(choice) }
                } label: {
                    HStack {
                        Text(choice.label)
                        Spacer()
                        if choice.selected { Image(systemName: "checkmark") }
                    }
                }
                .focused($nativeChoice, equals: "choice:" + choice.id)
                .remoteChoiceDefaultFocus(choice.selected || (!choices.contains(where: \.selected) && choice.id == choices.first?.id), in: focusNamespace)
                .id("choice:" + choice.id)
                .buttonStyle(RemoteChoiceRowStyle())
                .accessibilityIdentifier("choice:" + choice.id)
                .accessibilityAddTraits(choice.selected ? .isSelected : [])
                .remoteControl("choice:" + choice.id, label: choice.label, managesFocus: false, observesPhysicalActivation: false, admission: admission) {
                    commit(choice)
                }
            }
            Button("Close") { navigation.performPhysicalAction { if admission() == nil { navigation.closeModal() } } }
                .focused($nativeChoice, equals: "choice:close")
                .remoteChoiceDefaultFocus(choices.isEmpty, in: focusNamespace)
                .buttonStyle(RemoteChoiceRowStyle())
                .accessibilityIdentifier("choice:close")
                .id("choice:close")
                .remoteControl("choice:close", label: "Close", managesFocus: false, observesPhysicalActivation: false, admission: admission) { navigation.closeModal() }
        }
        .padding(30)
        }
        .frame(maxWidth: 600, maxHeight: 600)
        .background(Palette.surfaceHi, in: RoundedRectangle(cornerRadius: 18))
        .remoteChoiceFocusScope(focusNamespace)
        .defaultFocus($nativeChoice, choices.first(where: \.selected).map { "choice:" + $0.id } ?? choices.first.map { "choice:" + $0.id } ?? "choice:close", priority: .userInitiated)
        .remoteScope(scope)
        .onChange(of: navigation.focusRequestRevision) { _, _ in
            guard navigation.activeScope == scope, let key = navigation.requestedFocus else { return }
            nativeChoice = key
        }
        .task(id: navigation.focusRequestRevision) {
            guard !Task.isCancelled else { return }
            guard navigation.activeScope == scope, let key = navigation.requestedFocus else { return }
            nativeChoice = key
        }
        .onAppear(perform: enterPanel)
        .background(RemoteChoiceFocusReadiness(ready: nativePresentationReady).allowsHitTesting(false))
        }
    }
    private func commit(_ choice: RemoteChoice) {
        guard admission() == nil else { return }
        #if os(tvOS)
        guard navigation.choiceActionAdmitted(scope: scope, key: "choice:" + choice.id) else { return }
        #endif
        choice.choose()
        navigation.closeModal(returnFocus: choice.returnFocus)
    }
    private func nativePresentationReady() {
        #if os(tvOS)
        resetFocus(in: focusNamespace)
        #endif
        enterPanel()
    }
    private func enterPanel() {
        navigation.enterChoicePanel(scope: scope,
                                    keys: choices.map { "choice:" + $0.id } + ["choice:close"],
                                    selected: choices.first(where: \.selected).map { "choice:" + $0.id })
    }
}

/// Native appearance completion is the realization acknowledgment. SwiftUI's
/// onAppear runs before a full-screen cover can receive focus.
struct RemoteChoiceFocusReadiness: UIViewControllerRepresentable {
    let ready: () -> Void
    final class Controller: UIViewController {
        var ready: () -> Void = {}
        override func loadView() { view = UIView(); view.backgroundColor = .clear }
        override func viewDidAppear(_ animated: Bool) { super.viewDidAppear(animated); ready() }
    }
    func makeUIViewController(context: Context) -> Controller {
        let controller = Controller(); controller.ready = ready; return controller
    }
    func updateUIViewController(_ uiViewController: Controller, context: Context) { uiViewController.ready = ready }
}

/// An explicit owner marker authorizes only its actual UIKit presentation
/// chain. An unrelated sheet inside that cover is still blocked by the root.
struct RemoteOwnedPresentationProbe: UIViewRepresentable {
    let navigation: RemoteNavigationCoordinator
    let token: UUID
    let scope: String
    final class Probe: UIView {
        var attach: ((UIView) -> Void)?
        override func didMoveToWindow() { super.didMoveToWindow(); if window != nil { attach?(self) } }
    }
    func makeUIView(context: Context) -> Probe {
        let view = Probe()
        view.attach = { marker in
            var responder: UIResponder? = marker
            while let current = responder, !(current is UIViewController) { responder = current.next }
            var controller = responder as? UIViewController
            var identifiers: Set<ObjectIdentifier> = []
            while let current = controller {
                identifiers.insert(ObjectIdentifier(current))
                controller = current.parent
            }
            navigation.attachPresentation(token, scope: scope, controllers: identifiers)
        }
        return view
    }
    func updateUIView(_ uiView: Probe, context: Context) { if uiView.window != nil { uiView.attach?(uiView) } }
    static func dismantleUIView(_ uiView: Probe, coordinator: ()) { uiView.attach = nil }
}

/// Native presentation, not a focus section: UIKit excludes the covered screen
/// from its focus environment. The marker admits companion commands only to
/// this exact controller chain; nested unrelated presentations remain fenced.
private struct RemoteChoicePresentation<Panel: View>: ViewModifier {
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    @Binding var isPresented: Bool
    @State private var token = UUID()
    @State private var presentedScope: String?
    @State private var modalIdentity: UUID?
    let scope: String
    let onDismiss: () -> Void
    let panel: () -> Panel

    @ViewBuilder
    func body(content: Content) -> some View {
        #if os(tvOS)
        content.fullScreenCover(isPresented: $isPresented, onDismiss: {
            navigation.detachPresentation(token)
            navigation.modalPresentationDismissed(scope: presentedScope ?? scope, identity: modalIdentity)
            presentedScope = nil
            onDismiss()
        }) {
            ZStack {
                Palette.bg.ignoresSafeArea()
                panel()
            }
            .background(RemoteOwnedPresentationProbe(navigation: navigation, token: token, scope: scope))
            .interactiveDismissDisabled()
            .remotePhysicalDirections()
            .onAppear { presentedScope = scope; modalIdentity = navigation.ownedModalIdentity }
            .modifier(RemoteChoiceExitAdapter(navigation: navigation))
        }
        #else
        content.overlay { if isPresented { panel() } }
        #endif
    }
}

extension View {
    func remoteChoicePresentation<Panel: View>(
        isPresented: Binding<Bool>, scope: String, onDismiss: @escaping () -> Void = {},
        @ViewBuilder content: @escaping () -> Panel
    ) -> some View {
        modifier(RemoteChoicePresentation(isPresented: isPresented, scope: scope, onDismiss: onDismiss, panel: content))
    }
}

/// Own both sides of the contrast pair. Selection is a persistent checkmark;
/// focus is an outline, including when Reduce Motion suppresses the lift.
private struct RemoteChoiceRowStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View { Row(configuration: configuration) }
    private struct Row: View {
        let configuration: ButtonStyle.Configuration
        @Environment(\.isFocused) private var focused
        @Environment(\.isEnabled) private var enabled
        @Environment(\.accessibilityReduceMotion) private var reduceMotion
        var body: some View {
            configuration.label
                .foregroundStyle(Palette.onBg)
                .padding(.horizontal, 24)
                .padding(.vertical, 16)
                .frame(maxWidth: .infinity, minHeight: 66, alignment: .leading)
                .background(Palette.surfaceHi, in: RoundedRectangle(cornerRadius: 14))
                .overlay {
                    RoundedRectangle(cornerRadius: 14)
                        .stroke(focused ? Palette.onBg : Palette.outline, lineWidth: focused ? 4 : 1)
                }
                .opacity(enabled ? 1 : 0.45)
                .scaleEffect(reduceMotion ? 1 : (focused ? 1.025 : 1))
                .animation(reduceMotion ? nil : .easeOut(duration: 0.14), value: focused)
                .remoteNativeFocus(focused)
        }
    }
}

private extension View {
    @ViewBuilder
    func remoteChoiceDefaultFocus(_ preferred: Bool, in namespace: Namespace.ID) -> some View {
        #if os(tvOS)
        prefersDefaultFocus(preferred, in: namespace)
        #else
        self
        #endif
    }
    @ViewBuilder
    func remoteChoiceFocusScope(_ namespace: Namespace.ID) -> some View {
        #if os(tvOS)
        focusScope(namespace)
        #else
        self
        #endif
    }
}
