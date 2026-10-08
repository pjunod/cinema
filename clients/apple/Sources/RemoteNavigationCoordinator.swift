import Combine
import Foundation
import CoreGraphics

/// UI authorization is separate from B06's authenticated receiver admission.
/// Every callback here is a specifically registered browsing action.
@MainActor
final class RemoteNavigationCoordinator: ObservableObject {
    enum Direction { case up, down, left, right }
    enum Action {
        case navigate(Direction), select, back, home
        case textReplace(nonce: UUID, text: String)
    }
    enum Outcome: String { case applied, staleContext = "stale_context", staleFocus = "stale_focus", restrictedSurface = "restricted_surface", unsupported, invalid, unavailable }
    struct Context: Equatable {
        let epoch: UUID
        let contextRevision: UInt64
        let focusRevision: UInt64
    }
    struct Snapshot {
        let context: Context
        let scope: String
        let blocked: Bool
        let focusedLabel: String?
        let textNonce: UUID?
    }
    struct Entry {
        let id: UUID
        let label: String
        let frame: CGRect
        let activate: () -> Void
    }
    @Published private(set) var requestedFocus: String?
    @Published private(set) var contextRevision: UInt64 = 1
    @Published private(set) var focusRevision: UInt64 = 1
    @Published var selectedTab: HomeTab = .home
    @Published var paths: [HomeTab: [Route]] = [:]
    private(set) var epoch = UUID()
    private(set) var scope = "home"
    private(set) var focusedKey: String?
    private var entries: [String: [String: Entry]] = [:]
    private var orderedKeys: [String: [String]] = [:]
    private var columns: [String: Int] = [:]
    private var textContexts: [String: (UUID, (String) -> Void)] = [:]
    private var modal: (scope: String, returnFocus: String?, dismiss: () -> Void)?
    private var blockers: Set<UUID> = []
    private var exhausted = false
    private var restoredFocus: [String: String] = [:]
    private var liveScopes: Set<String> = []
    private let maximumScopes = 32
    private let maximumEntries = 512
    private let maximumOrderedKeys = 16_384
    private var pendingFocus = false
    private var focusedRegistration: UUID?
    var presentationBlocked: () -> Bool = { true }
    /// B06 cancels credits/holds before physical UI changes, never after dispatch.
    var onPhysicalInput: () -> Void = {}

    static func routeScope(_ route: Route) -> String {
        switch route {
        case .item(let id): return "item:\(id)"
        case .collection(let collection): return "library:\(collection.id)"
        }
    }
    var rootScope: String {
        if let route = paths[selectedTab]?.last { return Self.routeScope(route) }
        switch selectedTab {
        case .home: return "home"
        case .libraries: return "libraries"
        case .search: return "search"
        default: return "restricted"
        }
    }
    var activeScope: String { modal?.scope ?? scope }
    private var blocked: Bool {
        exhausted || scope == "restricted" || !blockers.isEmpty || presentationBlocked()
    }
    var context: Context { Context(epoch: epoch, contextRevision: contextRevision, focusRevision: focusRevision) }
    private func advanceContext() {
        guard contextRevision < 9_007_199_254_740_991 else { exhausted = true; return }
        contextRevision += 1
    }
    private func advanceFocus() {
        guard focusRevision < 9_007_199_254_740_991 else { exhausted = true; return }
        focusRevision += 1
    }
    func synchronizeRoute() {
        guard rootScope != scope else { return }
        modal?.dismiss()
        modal = nil
        if let focusedKey {
            if restoredFocus.count >= maximumScopes { restoredFocus.removeAll() }
            restoredFocus[scope] = focusedKey
        }
        scope = rootScope
        advanceContext()
        requestFocus(restoredFocus[scope])
    }
    /// Suspension retires command contexts without discarding the visible UI.
    func invalidate() {
        epoch = UUID()
        advanceContext()
        modal?.dismiss()
        modal = nil
        requestedFocus = nil
        focusedKey = nil
        focusedRegistration = nil
        pendingFocus = false
    }
    /// A credential/server transition must not retain another account's closures.
    func resetIdentity() {
        invalidate()
        entries.removeAll()
        orderedKeys.removeAll()
        columns.removeAll()
        textContexts.removeAll()
        restoredFocus.removeAll()
        liveScopes.removeAll()
        paths.removeAll()
        selectedTab = .home
        scope = "home"
    }
    func retainScope(_ scope: String) {
        if liveScopes.count < maximumScopes { liveScopes.insert(scope) }
    }
    func releaseScope(_ scope: String) {
        liveScopes.remove(scope)
        entries.removeValue(forKey: scope)
        orderedKeys.removeValue(forKey: scope)
        columns.removeValue(forKey: scope)
        textContexts.removeValue(forKey: scope)
    }
    func physicalInput() {
        onPhysicalInput()
        pendingFocus = false
        requestedFocus = focusedKey
        advanceFocus()
    }
    func setBlocked(_ token: UUID, _ value: Bool) {
        let changed = value ? blockers.insert(token).inserted : blockers.remove(token) != nil
        if changed { advanceContext(); onPhysicalInput() }
    }
    private func safeLabel(_ label: String) -> String {
        var result = ""
        for scalar in label.unicodeScalars {
            let next = String(scalar)
            if result.utf8.count + next.utf8.count > 256 { break }
            result += next
        }
        return result
    }
    func register(scope: String, key: String, entry: Entry) {
        guard entries[scope] != nil || entries.count < maximumScopes,
              entries[scope]?[key] != nil || (entries[scope]?.count ?? 0) < maximumEntries else { return }
        if activeScope == scope, focusedKey == key, focusedRegistration != entry.id {
            physicalFocus(scope: scope, key: nil)
        }
        entries[scope, default: [:]][key] = Entry(id: entry.id, label: safeLabel(entry.label), frame: entry.frame, activate: entry.activate)
    }
    func unregister(scope: String, key: String, id: UUID) {
        guard entries[scope]?[key]?.id == id else { return }
        entries[scope]?.removeValue(forKey: key)
        if activeScope == scope && focusedKey == key { physicalFocus(scope: scope, key: nil) }
        if entries[scope]?.isEmpty == true && !liveScopes.contains(scope) { releaseScope(scope) }
    }
    func setOrder(scope: String, keys: [String], columns: Int) {
        guard keys.count <= maximumOrderedKeys, Set(keys).count == keys.count,
              orderedKeys[scope] != nil || orderedKeys.count < maximumScopes else { return }
        if orderedKeys[scope] != keys {
            orderedKeys[scope] = keys
            if self.scope == scope { advanceContext() }
        }
        self.columns[scope] = max(1, columns)
    }
    func setSearch(scope: String, nonce: UUID, replace: @escaping (String) -> Void) {
        if textContexts[scope]?.0 != nonce && self.scope == scope { advanceContext() }
        textContexts[scope] = (nonce, replace)
    }
    func removeSearch(scope: String, nonce: UUID) {
        guard textContexts[scope]?.0 == nonce else { return }
        textContexts.removeValue(forKey: scope)
        if self.scope == scope { advanceContext() }
    }
    /// Only the actual native FocusState acknowledgment commits semantic focus.
    func physicalFocus(scope: String, key: String?) {
        guard scope == activeScope else { return }
        if pendingFocus && key == requestedFocus {
            pendingFocus = false
            focusedKey = key
            focusedRegistration = key.flatMap { entries[scope]?[$0]?.id }
            advanceFocus()
            return
        }
        guard key != focusedKey else { return }
        physicalInput()
        focusedKey = key
        focusedRegistration = key.flatMap { entries[scope]?[$0]?.id }
        requestedFocus = key
        advanceFocus()
    }
    private func requestFocus(_ key: String?) {
        if key == focusedKey && !pendingFocus { return }
        requestedFocus = key
        focusedKey = nil
        focusedRegistration = nil
        pendingFocus = key != nil
        advanceFocus()
    }
    func openModal(scope: String, opener: String, dismiss: @escaping () -> Void) {
        guard modal == nil else { return }
        modal = (scope, opener, dismiss)
        advanceContext()
        requestFocus(nil)
    }
    func closeModal() {
        guard let current = modal else { return }
        modal = nil
        current.dismiss()
        advanceContext()
        requestFocus(current.returnFocus)
    }
    func navigate(to route: Route) {
        paths[selectedTab, default: []].append(route)
        synchronizeRoute()
    }
    func snapshot() -> Snapshot {
        synchronizeRoute()
        let restricted = blocked
        return Snapshot(context: context, scope: restricted ? "restricted" : activeScope,
                        blocked: restricted, focusedLabel: restricted ? nil : focusedKey.flatMap { entries[activeScope]?[$0]?.label },
                        textNonce: restricted || modal != nil ? nil : textContexts[scope]?.0)
    }
    private func spatialTarget(from key: String, direction: Direction) -> String? {
        guard let current = entries[activeScope]?[key], !current.frame.isEmpty else { return nil }
        let candidates = (entries[activeScope] ?? [:]).compactMap { candidateKey, entry -> (String, CGFloat)? in
            guard candidateKey != key, !entry.frame.isEmpty,
                  orderedKeys[activeScope]?.contains(candidateKey) == true else { return nil }
            let dx = entry.frame.midX - current.frame.midX
            let dy = entry.frame.midY - current.frame.midY
            let forward: CGFloat
            let sideways: CGFloat
            switch direction {
            case .up: forward = -dy; sideways = abs(dx)
            case .down: forward = dy; sideways = abs(dx)
            case .left: forward = -dx; sideways = abs(dy)
            case .right: forward = dx; sideways = abs(dy)
            }
            guard forward > 1 else { return nil }
            return (candidateKey, forward + sideways * 3)
        }
        return candidates.sorted { $0.1 == $1.1 ? $0.0 < $1.0 : $0.1 < $1.1 }.first?.0
    }
    func dispatch(_ action: Action, context expected: Context) -> Outcome {
        synchronizeRoute()
        guard !blocked else { return .restrictedSurface }
        guard expected.epoch == epoch, expected.contextRevision == contextRevision else { return .staleContext }
        switch action {
        case .select:
            guard expected.focusRevision == focusRevision else { return .staleFocus }
            guard !pendingFocus, let key = focusedKey, let entry = entries[activeScope]?[key],
                  focusedRegistration == entry.id, !entry.frame.isEmpty else { return .unsupported }
            // Copying the registered closure never falls back to native key injection.
            advanceFocus()
            entry.activate()
        case .navigate(let direction):
            let keys = orderedKeys[activeScope] ?? []
            guard !keys.isEmpty else { return .unsupported }
            let index = (requestedFocus ?? focusedKey).flatMap { keys.firstIndex(of: $0) }
            let step: Int
            switch direction {
            case .up: step = -(columns[activeScope] ?? 1)
            case .down: step = columns[activeScope] ?? 1
            case .left: step = -1
            case .right: step = 1
            }
            let next = index.map { min(keys.count - 1, max(0, $0 + step)) } ?? 0
            let currentKey = requestedFocus ?? focusedKey
            let target = currentKey.flatMap { spatialTarget(from: $0, direction: direction) } ?? keys[next]
            requestFocus(target)
        case .back:
            if modal != nil { closeModal() }
            else if !(paths[selectedTab] ?? []).isEmpty {
                paths[selectedTab]?.removeLast()
                synchronizeRoute()
            } else { return .unsupported }
        case .home:
            guard modal == nil else { return .unsupported }
            selectedTab = .home
            paths[.home] = []
            synchronizeRoute()
        case .textReplace(let nonce, let text):
            guard modal == nil, let field = textContexts[scope], field.0 == nonce else { return .staleContext }
            guard text.utf8.count <= 512 else { return .invalid }
            field.1(text)
        }
        return .applied
    }
}
