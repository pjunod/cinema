import Combine
import Foundation
import UIKit

@MainActor
final class RemoteClientModel: ObservableObject {
    @Published private(set) var status = "Cinema remotes are disabled on this installation."
    @Published private(set) var devices: [CinemaRemoteDevice] = []
    @Published private(set) var unavailableNodes: [String] = []
    @Published private(set) var target: CinemaRemoteTarget?
    @Published private(set) var challenge: CinemaRemoteChallenge?
    @Published private(set) var pairingOpening = false
    @Published private(set) var pairingExpired = false
    @Published private(set) var pairings: [CinemaRemotePendingPairing] = []
    @Published private(set) var selectedDevice: CinemaRemoteDevice?
    @Published private(set) var controllerState: CinemaRemoteState?
    @Published private(set) var controllerControl: CinemaRemoteControl?
    @Published private(set) var controlling = false
    @Published private(set) var commandStatus = ""
    @Published private(set) var pairedGrants: [CinemaRemoteGrantInfo] = []
    @Published private(set) var managementStatus = ""
    @Published var remotePresented = false
    @Published var choosingDevice = false
    private var api: CinemaRemoteAPI?
    private var storage: RemoteSecretStorage?
    private var navigation: RemoteNavigationCoordinator?
    private var playback: RemotePlaybackAdapter?
    private let guardState = RemoteReceiverGuard()
    private let clock = RemoteMonotonicClock()
    private var receiver: RemoteSecretStorage.Receiver?
    private var receiverControl: CinemaRemoteControl?
    private var receiverTask: Task<Void, Never>?
    private var deferredTask: Task<Void, Never>?
    private var deferredExpiryTask: Task<Void, Never>?
    private var deferredID = UUID()
    private var presenceTask: Task<Void, Never>?
    private var discoveryTask: Task<Void, Never>?
    private var controllerTask: Task<Void, Never>?
    private var renewTask: Task<Void, Never>?
    private var pairingTask: Task<Void, Never>?
    private var pairingLifetime = RemotePairingLifetime()
    private var receiverPairingLifetime = RemotePairingLifetime()
    private var receiverPairingTask: Task<Void, Never>?
    private var challengeExpiryTask: Task<Void, Never>?
    private var phonePairingExpiryTask: Task<Void, Never>?
    private var commandResult = RemoteCommandResult()
    private var holdTask: Task<Void, Never>?
    private var authObserver: UUID?
    private var lifecycle = UUID()
    private var controllerGeneration = UUID()
    private var active = false
    private var enabled = false
    private var scene = RemoteSceneEligibility()
    private var uiRevision: UInt64 = 1
    private var controllerRevision: UInt64 = 0
    private var nextSequence: UInt64 = 0
    private var sequences = RemoteControlSequence()
    private var controlEligibility = RemoteControlEligibility()
    private var selectedGrant: RemoteSecretStorage.Grant?
    private var dismissedSuggestions: Set<UUID> = []
    private var sending = false
    private var sendToken = UUID()
    private var identity: String?
    var serverInstanceID: String? { SettingsStore().instanceId }

    init(navigation: RemoteNavigationCoordinator? = nil) {
        self.navigation = navigation
        authObserver = Session.shared.observeAuthorizationChanges { [weak self] generation in
            Task { @MainActor in
                guard self?.api?.generation != generation else { return }
                self?.storage?.clear()
                self?.shutdown(identityChange: true)
            }
        }.id
    }
    func configure(model: AppModel, navigation: RemoteNavigationCoordinator, playback: RemotePlaybackAdapter,
                   foreground: Bool, background: Bool, enabled: Bool) {
        self.navigation = navigation
        self.playback = playback
        navigation.onPhysicalInput = { [weak self] in self?.guardState.invalidate(); self?.playback?.physicalInput() }
        self.enabled = enabled
        scene.transition(active: foreground, background: background)
        guard foreground, enabled, model.phase == .ready,
              let userID = model.userId, let instance = SettingsStore().instanceId,
              let bearer = Session.shared.credentials.token else {
            shutdown(identityChange: false)
            status = enabled ? "Waiting for an authenticated foreground Cinema session." : "Cinema remotes are disabled on this installation."
            return
        }
        let nextIdentity = instance + ":" + String(userID)
        let auth = Session.shared.playbackAuthorization
        if active, identity == nextIdentity, api?.generation == auth.generation { return }
        let changedAuthorization = api != nil && api?.generation != auth.generation
        shutdown(identityChange: (identity != nil && identity != nextIdentity) || changedAuthorization)
        active = true
        identity = nextIdentity
        storage = RemoteSecretStorage(identity: nextIdentity)
        api = CinemaRemoteAPI(origin: Session.canonicalOrigin(model.origin) ?? "", token: bearer, generation: auth.generation)
        let generation = lifecycle
        discoveryTask = Task { [weak self] in await self?.discoverLoop(generation) }
        #if os(tvOS)
        receiverTask = Task { [weak self] in await self?.receiverLoop(generation) }
        #endif
    }
    func shutdown(identityChange: Bool) {
        active = false
        lifecycle = UUID()
        receiverTask?.cancel(); receiverTask = nil
        presenceTask?.cancel(); presenceTask = nil
        discoveryTask?.cancel(); discoveryTask = nil
        pairingTask?.cancel(); pairingTask = nil
        hidePairing()
        closeController()
        deferredID = UUID(); deferredTask?.cancel(); deferredTask = nil
        deferredExpiryTask?.cancel(); deferredExpiryTask = nil; playback?.physicalInput()
        guardState.deactivate()
        target = nil
        receiverControl = nil
        receiver = nil
        challenge = nil
        pairings = []
        devices = []
        pairedGrants = []
        unavailableNodes = []
        if identityChange { storage?.clear(); storage = nil; identity = nil; navigation?.resetIdentity(); sequences = RemoteControlSequence() }
    }
    func sceneChanged(active: Bool, background: Bool) {
        scene.transition(active: active, background: background)
        if !active { guardState.invalidate(); playback?.physicalInput(); shutdown(identityChange: false) }
    }
    private var sceneCanAct: Bool { scene.eligible && UIApplication.shared.applicationState == .active }
    private func current(_ generation: UUID) -> Bool { sceneCanAct && active && lifecycle == generation && api?.isCurrent == true && !Task.isCancelled }
    private func discoverLoop(_ generation: UUID) async {
        while current(generation) {
            do {
                guard let api else { return }
                let response = try await api.devices()
                guard current(generation) else { return }
                devices = Array(response.receivers.prefix(100))
                unavailableNodes = Array(response.unavailableNodes.prefix(32))
                status = unavailableNodes.isEmpty ? "Foreground remote discovery is connected." : "Some server nodes are unavailable."
                if let selectedDevice, let refreshed = devices.first(where: { $0.id == selectedDevice.id }) {
                    if refreshed.target != selectedDevice.target || !refreshed.available { closeController() }
                }
            } catch {
                guard current(generation) else { return }
                status = "Remote discovery is unavailable. Check the server's Cinema remote setting and connection."
            }
            try? await Task.sleep(for: .seconds(5))
        }
    }
    private func receiverLoop(_ generation: UUID) async {
        var retry = 1.0
        while current(generation) {
            do {
                guard let api, let storage else { return }
                if receiver == nil {
                    if let saved = storage.receiver { receiver = saved }
                    else {
                        let created = try await api.register(name: "Apple TV")
                        guard current(generation) else { return }
                        let saved = RemoteSecretStorage.Receiver(id: created.receiverID, secret: created.receiverSecret)
                        try storage.saveReceiver(saved)
                        receiver = saved
                    }
                }
                guard let receiver else { return }
                let session = try await api.session(receiverID: receiver.id, foregroundID: scene.foregroundID, secret: receiver.secret)
                guard current(generation) else { return }
                target = session.target
                guardState.deactivate()
                receiverControl = nil
                var cursor: UInt64 = 0
                var responseRevision: UInt64 = 0
                let capturedTarget = session.target
                presenceTask?.cancel()
                presenceTask = Task { [weak self] in await self?.presenceLoop(generation, target: capturedTarget, receiver: receiver) }
                retry = 1
                while current(generation), target == capturedTarget {
                    let batch = try await api.poll(target: capturedTarget, after: cursor, responseRevision: responseRevision, secret: receiver.secret)
                    guard current(generation), batch.target == capturedTarget, target == capturedTarget else { return }
                    guard batch.responseRevision >= responseRevision else { continue }
                    responseRevision = batch.responseRevision
                    pairings = Array(batch.pairings.prefix(8))
                    if receiverControl != batch.control {
                        guardState.invalidate(); playback?.physicalInput()
                        presenceTask?.cancel()
                        receiverControl = batch.control
                        presenceTask = Task { [weak self] in await self?.presenceLoop(generation, target: capturedTarget, receiver: receiver) }
                    }
                    if batch.control == nil { guardState.deactivate() }
                    var outcomes: [RemoteReceiverGuard.Acknowledgement] = []
                    for command in batch.commands {
                        if let outcome = receive(command) {
                            outcomes.append(.init(controlEpoch: command.controlEpoch, sequence: command.sequence, outcome: outcome))
                        }
                    }
                    cursor = max(cursor, batch.deliveryID)
                    if !outcomes.isEmpty {
                        _ = try await api.ack(target: capturedTarget, outcomes: outcomes, secret: receiver.secret)
                        guard current(generation) else { return }
                    }
                }
            } catch {
                guard current(generation) else { return }
                guardState.deactivate()
                presenceTask?.cancel(); presenceTask = nil
                hidePairing()
                target = nil
                receiverControl = nil
                challenge = nil
                pairings = []
                status = "TV remote session disconnected; reconnecting with a fresh target."
                try? await Task.sleep(for: .seconds(retry + Double.random(in: 0...0.5)))
                retry = min(30, retry * 2)
            }
        }
    }
    private func updateGuard() throws {
        guard let navigation, let target, let control = receiverControl else { throw CinemaRemoteOutcome.unavailable }
        let state = navigation.snapshot()
        try guardState.setContext(.init(grantID: control.activeGrantID, target: target, controlEpoch: control.controlEpoch,
                                        contextRevision: state.context.contextRevision, focusRevision: state.context.focusRevision, textNonce: state.textNonce))
    }
    private func presenceLoop(_ generation: UUID, target: CinemaRemoteTarget, receiver: RemoteSecretStorage.Receiver) async {
        while current(generation), self.target == target {
            do {
                guard let api else { return }
                let state = makeState()
                _ = try await api.presence(target: target, state: state, secret: receiver.secret)
                guard current(generation), self.target == target else { return }
            } catch {
                guard current(generation) else { return }
                guardState.invalidate()
                status = "TV presence could not refresh; old commands remain expired."
            }
            try? await Task.sleep(for: .milliseconds(receiverControl == nil ? 5_000 : 250))
        }
    }
    private func makeState() -> CinemaRemoteState { makeState(pairingRestricted: localPairingRestricted) }
    /// The presentation restriction is captured once for each publication.
    func makeState(pairingRestricted: Bool) -> CinemaRemoteState {
        guard uiRevision < CinemaRemoteCommand.maximumInteger else { guardState.deactivate(); return restrictedState() }
        uiRevision += 1
        guard let navigation else { return restrictedState() }
        let snapshot = navigation.snapshot()
        if pairingRestricted { guardState.invalidate(); return restrictedState() }
        if snapshot.blocked { guardState.invalidate(); return restrictedState() }
        if receiverControl != nil {
            do {
                try updateGuard()
                _ = try guardState.mint(.interaction, now: clock.milliseconds)
                _ = try guardState.mint(.playback, now: clock.milliseconds)
            } catch { guardState.invalidate() }
        }
        var actions: [CinemaRemoteAction.Kind] = [.navigate, .select, .back, .home, .playItem]
        if snapshot.scope.contains(":shared") { actions.removeAll { $0 == .playItem } }
        if snapshot.textNonce != nil { actions.append(.textReplace) }
        var summary: CinemaRemotePlaybackSummary?
        if let owner = playback?.owner, owner.available(), snapshot.scope.hasPrefix(owner.scope) {
            actions += owner.actions.filter { owner.actionAvailable($0) && !actions.contains($0) }.sorted { $0.rawValue < $1.rawValue }
            summary = owner.snapshot()
        }
        let route: String
        if snapshot.scope.contains(":tracks") { route = "tracks" }
        else if snapshot.scope.hasPrefix("playback:") || snapshot.scope == "live-tv" { route = "playback" }
        else if snapshot.scope.hasPrefix("library:") || snapshot.scope == "libraries" { route = "library" }
        else if snapshot.scope.hasPrefix("item:") { route = "details" }
        else { route = snapshot.scope == "search" ? "search" : "home" }
        return RemoteStateBudget.fit(.init(stateRevision: uiRevision, contextRevision: snapshot.context.contextRevision, focusRevision: snapshot.context.focusRevision,
                     route: route, capabilities: Array(actions.prefix(12)), focusedLabel: snapshot.focusedLabel,
                     credits: guardState.currentCredits, textNonce: snapshot.textNonce, playback: summary))
    }
    private func restrictedState() -> CinemaRemoteState {
        let context = navigation?.context
        return .init(stateRevision: max(1, uiRevision), contextRevision: context?.contextRevision ?? 1,
                     focusRevision: context?.focusRevision ?? 1, route: "restricted", capabilities: [], focusedLabel: nil, credits: [], textNonce: nil, playback: nil)
    }
    /// Deferred shared effects never block the serial receiver poll/control loop.
    private func receive(_ command: CinemaRemoteCommand) -> CinemaRemoteOutcome? {
        guard let navigation, let owner = playback?.owner, let effect = owner.deferredDispatch,
              navigation.snapshot().scope == owner.scope, owner.actions.contains(command.action.type) else { return apply(command) }
        guard current(lifecycle), let target, let receiver, let api else { return .unavailable }
        if localPairingRestricted { guardState.invalidate(); return .restrictedSurface }
        do { try updateGuard() } catch let outcome as CinemaRemoteOutcome { return outcome } catch { return .invalid }
        if guardState.isPending(command, now: clock.milliseconds) { return nil }
        if let known = guardState.result(controlEpoch: command.controlEpoch, sequence: command.sequence, now: clock.milliseconds) { return known.outcome }
        let snapshot = navigation.snapshot()
        let semantic: CinemaRemoteOutcome? = snapshot.blocked ? .restrictedSurface : (owner.available() && owner.actionAvailable(command.action.type) ? nil : .unavailable)
        switch guardState.reserve(command, owner: owner.token, now: clock.milliseconds, semantic: semantic, replacingPending: [.stop, .back, .home].contains(command.action.type)) {
        case .rejected(let outcome): return outcome
        case .admitted(let permit):
            deferredTask?.cancel(); deferredExpiryTask?.cancel()
            if [.stop, .back, .home].contains(command.action.type) { owner.cancelNetworkGesture() }
            let id = UUID(); deferredID = id
            let generation = lifecycle
            func stillOwned() -> Bool {
                self.current(generation) && self.target == target && self.deferredID == id &&
                self.playback?.owner?.token == owner.token && self.receiverControl?.controlEpoch == command.controlEpoch &&
                self.receiverControl?.activeGrantID == command.grantID && navigation.snapshot().context == snapshot.context &&
                navigation.activeScope == owner.scope && !navigation.snapshot().blocked && !self.localPairingRestricted
            }
            deferredExpiryTask = Task {
                do { try await Task.sleep(for: .seconds(10)) } catch { return }
                guard deferredID == id else { return }
                guardState.retire(permit); deferredTask?.cancel(); owner.cancelNetworkGesture()
            }
            deferredTask = Task {
                let result = await effect(command.action) {
                    stillOwned() && self.guardState.permitsDispatch(permit, owner: owner.token, now: self.clock.milliseconds)
                }
                guard stillOwned(), let acknowledgement = guardState.complete(permit, owner: owner.token, now: clock.milliseconds, outcome: result) else { return }
                deferredExpiryTask?.cancel(); deferredExpiryTask = nil
                owner.deferredDidComplete(command.action, result)
                _ = try? await api.ack(target: target, outcomes: [acknowledgement], secret: receiver.secret)
                // No replay or inferred rollback after either B or ACK uncertainty.
            }
            return nil
        }
    }
    private func apply(_ command: CinemaRemoteCommand) -> CinemaRemoteOutcome {
        guard sceneCanAct, active, let navigation, api?.isCurrent == true else { return .unavailable }
        if localPairingRestricted { guardState.invalidate(); return .restrictedSurface }
        do { try updateGuard() } catch let outcome as CinemaRemoteOutcome { return outcome } catch { return .invalid }
        let snapshot = navigation.snapshot()
        let semantic: CinemaRemoteOutcome? = snapshot.blocked ? .restrictedSurface : nil
        return guardState.apply(command, now: clock.milliseconds, semantic: semantic) {
            self.dispatch(command.action, context: snapshot.context)
        }
    }
    private func dispatch(_ action: CinemaRemoteAction, context: RemoteNavigationCoordinator.Context) -> CinemaRemoteOutcome {
        guard let navigation else { return .unavailable }
        let snapshot = navigation.snapshot()
        guard !snapshot.blocked else { return .restrictedSurface }
        if let owner = playback?.owner, snapshot.scope.hasPrefix(owner.scope) {
            if action.type == .openTracks || action.type == .chooseTrack || [.setPlaying, .seekRelative, .seekAbsolute, .stop].contains(action.type) {
                return playback?.dispatch(action, scope: owner.scope) ?? .unsupported
            }
            if !navigation.hasOwnedModal && [.navigate, .select, .back].contains(action.type) {
                let outcome = playback?.dispatch(action, scope: owner.scope) ?? .unsupported
                if outcome != .unsupported { return outcome }
            }
            if action.type == .home {
                let result = playback?.dispatch(.init(type: .stop), scope: owner.scope) ?? .unsupported
                guard result == .applied else { return result }
            }
        }
        let routed: RemoteNavigationCoordinator.Action
        switch action.type {
        case .navigate:
            guard let direction = action.direction else { return .invalid }
            switch direction { case .up: routed = .navigate(.up); case .down: routed = .navigate(.down); case .left: routed = .navigate(.left); case .right: routed = .navigate(.right) }
        case .select: routed = .select
        case .back: routed = .back
        case .home: routed = .home
        case .textReplace:
            guard let nonce = action.textNonce, let text = action.text else { return .invalid }
            routed = .textReplace(nonce: nonce, text: text)
        case .playItem:
            guard let id = action.itemID, id <= UInt64(Int.max) else { return .invalid }
            navigation.requestPlayback(itemID: Int(id))
            return .applied
        default: return .unsupported
        }
        return CinemaRemoteOutcome(rawValue: navigation.dispatch(routed, context: context).rawValue) ?? .invalid
    }

    private var localPairingRestricted: Bool { pairingOpening || pairingExpired || challenge != nil || !pairings.isEmpty }
    func startPairing() {
        guard let api, let target, let receiver, current(lifecycle) else { return }
        receiverPairingTask?.cancel(); challengeExpiryTask?.cancel()
        let ticket = receiverPairingLifetime.begin(receiverID: receiver.id, target: target)
        let generation = lifecycle
        let started = clock.milliseconds
        challenge = nil; pairingExpired = false; pairingOpening = true; guardState.invalidate()
        receiverPairingTask = Task {
            do {
                let value = try await api.pairingStart(target: target, secret: receiver.secret)
                guard current(generation), receiverPairingLifetime.accepts(ticket, receiverID: self.receiver?.id, target: self.target) else { return }
                guard value.target == target, value.expiresInMs > 0, value.expiresInMs <= 120_000 else { throw CinemaRemoteOutcome.invalid }
                let deadline = RemotePairingDeadline(start: started, budget: value.expiresInMs)
                pairingOpening = false
                guard deadline.admits(clock.milliseconds) else { pairingExpired = true; return }
                challenge = value
                challengeExpiryTask = Task {
                    do { try await Task.sleep(for: .milliseconds(Int64(deadline.remaining(clock.milliseconds)))) } catch { return }
                    guard current(generation), receiverPairingLifetime.accepts(ticket, receiverID: self.receiver?.id, target: self.target) else { return }
                    challenge = nil; pairingExpired = true; guardState.invalidate()
                }
            } catch { if current(generation), receiverPairingLifetime.accepts(ticket, receiverID: self.receiver?.id, target: self.target) { pairingOpening = false; status = "Pairing is unavailable." } }
        }
    }
    func approve(_ pending: CinemaRemotePendingPairing, allow: Bool) {
        guard let api, let target, let receiver, current(lifecycle) else { return }
        let generation = lifecycle
        let ticket = receiverPairingLifetime.begin(receiverID: receiver.id, target: target)
        receiverPairingTask?.cancel(); challengeExpiryTask?.cancel(); pairingOpening = false
        // Approval retires the advertised code. A failed approval offers a fresh
        // challenge rather than retaining a code whose expiry timer was retired.
        challenge = nil; pairingExpired = true; guardState.invalidate()
        receiverPairingTask = Task {
            do {
                _ = try await api.pairingApprove(target: target, pendingID: pending.id, approve: allow, secret: receiver.secret)
                guard current(generation), receiverPairingLifetime.accepts(ticket, receiverID: self.receiver?.id, target: self.target) else { return }
                pairings.removeAll { $0.id == pending.id }; challenge = nil; pairingExpired = false
            } catch { if current(generation), receiverPairingLifetime.accepts(ticket, receiverID: self.receiver?.id, target: self.target) { status = "Pairing approval could not be confirmed." } }
        }
    }
    func hidePairing() {
        receiverPairingLifetime.retire(); receiverPairingTask?.cancel(); receiverPairingTask = nil
        challengeExpiryTask?.cancel(); challengeExpiryTask = nil
        challenge = nil; pairingOpening = false; pairingExpired = false; guardState.invalidate()
    }
    func pair(device: CinemaRemoteDevice, challengeID: UUID?, code: String) {
        guard let api, let target = device.target, let storage,
              selectedDevice?.id == device.id, selectedDevice?.target == target else { return }
        pairingTask?.cancel()
        let ticket = pairingLifetime.begin(receiverID: device.id, target: target)
        let generation = lifecycle
        let deadline = RemotePairingDeadline(start: clock.milliseconds)
        let expiredMessage = "Pairing expired. Start a new TV code and try again."
        func stillPairing() -> Bool { current(generation) && pairingLifetime.accepts(ticket, receiverID: selectedDevice?.id, target: selectedDevice?.target) }
        phonePairingExpiryTask?.cancel()
        phonePairingExpiryTask = Task {
            do { try await Task.sleep(for: .milliseconds(Int64(deadline.remaining(clock.milliseconds)))) } catch { return }
            guard stillPairing() else { return }
            commandStatus = expiredMessage; pairingTask?.cancel(); pairingTask = nil; pairingLifetime.retire()
        }
        pairingTask = Task {
            defer {
                if pairingLifetime.accepts(ticket, receiverID: selectedDevice?.id, target: selectedDevice?.target) {
                    phonePairingExpiryTask?.cancel(); phonePairingExpiryTask = nil
                }
            }
            do {
                let claim = try await api.pairingClaim(target: target, challengeID: challengeID, code: code, name: "Cinema phone")
                guard stillPairing() else { return }
                guard deadline.admits(clock.milliseconds) else { commandStatus = expiredMessage; return }
                for _ in 0..<120 {
                    try await Task.sleep(for: .seconds(1))
                    guard stillPairing() else { return }
                    guard deadline.admits(clock.milliseconds) else { commandStatus = expiredMessage; return }
                    let result = try await api.pairingResult(target: target, pendingID: claim.pendingID, secret: claim.pollSecret)
                    guard stillPairing() else { return }
                    guard deadline.admits(clock.milliseconds) else { commandStatus = expiredMessage; return }
                    if result.status == "denied" { commandStatus = "Pairing was declined on the TV."; return }
                    if result.status == "approved" {
                        guard let id = result.grantID, let secret = result.grantSecret, result.receiverID == device.id else { throw CinemaRemoteOutcome.invalid }
                        guard deadline.admits(clock.milliseconds) else { commandStatus = expiredMessage; return }
                        try storage.saveGrant(.init(receiverID: device.id, id: id, secret: secret))
                        commandStatus = "Paired. Tap Use as remote to take control."
                        pairingTask = nil
                        select(device)
                        return
                    }
                    commandStatus = "Waiting for local approval on the TV."
                }
                if stillPairing() { commandStatus = expiredMessage }
            } catch { if current(generation), pairingLifetime.accepts(ticket, receiverID: selectedDevice?.id, target: selectedDevice?.target) { commandStatus = "Pairing could not finish. Start a new TV code." } }
        }
    }
    func select(_ device: CinemaRemoteDevice) {
        closeController()
        guard device.available, device.target != nil, let grant = storage?.grants.first(where: { $0.receiverID == device.id }) else {
            selectedDevice = device
            commandStatus = "Pair this phone using the code shown on the TV."
            return
        }
        selectedDevice = device
        selectedGrant = grant
        remotePresented = true
        beginStatePoll()
    }
    var controlledByOtherPhone: Bool { controllerControl != nil && controllerControl?.activeGrantID != selectedGrant?.id }
    var selectedIsPaired: Bool { selectedGrant != nil }
    func closeController() {
        pairingLifetime.retire()
        phonePairingExpiryTask?.cancel(); phonePairingExpiryTask = nil
        pairingTask?.cancel(); pairingTask = nil
        controlEligibility.retire()
        controllerGeneration = UUID()
        commandResult.retire()
        controllerTask?.cancel(); controllerTask = nil
        renewTask?.cancel(); renewTask = nil
        stopHolding()
        controlling = false
        controllerState = nil
        controllerControl = nil
        controllerRevision = 0
        nextSequence = 0
        selectedDevice = nil
        selectedGrant = nil
        sending = false; sendToken = UUID()
    }
    func acquire(takeover: Bool = false) {
        guard let api, let device = selectedDevice, let target = device.target, let grant = selectedGrant else { return }
        let recoveryEpoch = sequences.recoveryEpoch(target: target, grant: grant.id, control: controllerControl)
        let renewUnknownEpoch = recoveryEpoch != nil
        controlEligibility.retire()
        controllerGeneration = UUID()
        commandResult.retire()
        controllerTask?.cancel(); renewTask?.cancel(); stopHolding()
        controlling = false; sending = false; sendToken = UUID(); controllerState = nil
        let generation = controllerGeneration
        let life = lifecycle
        Task {
            do {
                let requestsFreshEpoch = takeover || renewUnknownEpoch
                var reply = try await api.control(target: target, grantID: grant.id, action: requestsFreshEpoch ? "takeover" : "acquire", epoch: takeover ? nil : recoveryEpoch, secret: grant.secret)
                guard current(life), generation == controllerGeneration, selectedDevice?.target == target, reply.target == target else { return }
                // An acquire may renew our pre-existing lease even before state
                // polling discovers it. Explicit Use as remote permits replacing
                // our own unknown allocator with a fresh epoch, never another grant.
                if !requestsFreshEpoch, let expected = sequences.recoveryEpoch(target: target, grant: grant.id, control: reply.control) {
                    reply = try await api.control(target: target, grantID: grant.id, action: "takeover", epoch: expected, secret: grant.secret)
                    guard current(life), generation == controllerGeneration, selectedDevice?.target == target, reply.target == target else { return }
                }
                controlEligibility.acquired(generation)
                acceptControl(reply.control, revision: reply.responseRevision, acquired: true)
                beginStatePoll()
                if controlling { beginRenewal() }
            } catch {
                guard current(life), generation == controllerGeneration else { return }
                commandStatus = "Control could not be acquired. Another phone may hold the lease."
                beginStatePoll()
            }
        }
    }
    private func acceptControl(_ control: CinemaRemoteControl?, revision: UInt64, acquired: Bool = false) {
        guard revision >= controllerRevision else { return }
        controllerRevision = revision
        if control?.controlEpoch != controllerControl?.controlEpoch { nextSequence = 0; controllerState = nil; sending = false; sendToken = UUID(); stopHolding() }
        controllerControl = control
        if let control, let target = selectedDevice?.target, let grant = selectedGrant, control.activeGrantID == grant.id {
            if acquired { sequences.acquired(target: target, grant: grant.id, epoch: control.controlEpoch) }
            controlling = controlEligibility.permits(controllerGeneration) && sequences.knows(target: target, grant: grant.id, epoch: control.controlEpoch)
        } else { controlling = false }
        if !controlling { controlEligibility.retire(); stopHolding() }
    }
    /// B04 null state means unchanged, including an empty timeout response.
    func acceptState(_ state: CinemaRemoteState?) {
        guard let state else { return }
        if controllerState?.contextRevision != state.contextRevision { stopHolding() }
        controllerState = state
    }
    private func beginStatePoll() {
        controllerTask?.cancel()
        let generation = controllerGeneration
        let life = lifecycle
        controllerTask = Task {
            guard let api, let device = selectedDevice, let target = device.target, let grant = selectedGrant else { return }
            while current(life), generation == controllerGeneration {
                do {
                    let reply = try await api.state(target: target, grantID: grant.id, after: controllerRevision, secret: grant.secret)
                    guard current(life), generation == controllerGeneration, selectedDevice?.target == target, reply.target == target else { return }
                    guard reply.responseRevision >= controllerRevision else { continue }
                    acceptControl(reply.control, revision: reply.responseRevision)
                    acceptState(reply.state)
                    if let last = reply.outcomes.last(where: { $0.controlEpoch == controllerControl?.controlEpoch && $0.sequence == nextSequence }), commandResult.observe(epoch: last.controlEpoch, sequence: last.sequence, outcome: last.outcome) { commandStatus = last.outcome.viewerMessage }
                } catch {
                    guard current(life), generation == controllerGeneration else { return }
                    controlling = false; controlEligibility.retire()
                    stopHolding()
                    commandStatus = "Remote connection unavailable; no command is replayed."
                    try? await Task.sleep(for: .seconds(1))
                }
            }
        }
    }
    private func beginRenewal() {
        renewTask?.cancel()
        let generation = controllerGeneration
        let life = lifecycle
        renewTask = Task {
            guard let api, let device = selectedDevice, let target = device.target, let grant = selectedGrant else { return }
            while current(life), generation == controllerGeneration, controlling {
                try? await Task.sleep(for: .seconds(5))
                guard current(life), generation == controllerGeneration, controlling, let epoch = controllerControl?.controlEpoch else { return }
                do {
                    let reply = try await api.control(target: target, grantID: grant.id, action: "renew", epoch: epoch, secret: grant.secret)
                    guard current(life), generation == controllerGeneration, reply.target == target else { return }
                    acceptControl(reply.control, revision: reply.responseRevision)
                } catch { if generation == controllerGeneration { controlling = false; controlEligibility.retire(); stopHolding(); commandStatus = "Control lease renewal failed." }; return }
            }
        }
    }
    func releaseControl() {
        guard let api, let target = selectedDevice?.target, let grant = selectedGrant, let epoch = controllerControl?.controlEpoch else { closeController(); return }
        controlEligibility.retire()
        controllerGeneration = UUID()
        commandResult.retire()
        controllerTask?.cancel(); renewTask?.cancel(); stopHolding()
        controlling = false; sending = false; sendToken = UUID()
        let generation = controllerGeneration
        let life = lifecycle
        Task {
            _ = try? await api.control(target: target, grantID: grant.id, action: "release", epoch: epoch, secret: grant.secret)
            guard current(life), generation == controllerGeneration else { return }
            closeController()
        }
    }
    func send(_ action: CinemaRemoteAction) {
        guard sceneCanAct, active, !sending, controlling, let api, let target = selectedDevice?.target, let grant = selectedGrant,
              let control = controllerControl, let state = controllerState, state.capabilities.contains(action.type),
              let credit = state.credits.last(where: { $0.kind == action.creditKind }) else { return }
        guard (try? action.validate()) != nil else { commandStatus = "Command parameters are invalid."; return }
        guard let sequence = sequences.next(target: target, grant: grant.id, epoch: control.controlEpoch) else { controlling = false; return }
        nextSequence = sequence
        commandResult.begin(epoch: control.controlEpoch, sequence: sequence)
        let command = CinemaRemoteCommand(target: target, grantID: grant.id, controlEpoch: control.controlEpoch, sequence: sequence,
                                          credit: credit.nonce, contextRevision: state.contextRevision, focusRevision: state.focusRevision, action: action)
        sending = true
        let token = UUID(); sendToken = token
        let generation = controllerGeneration
        let life = lifecycle
        Task {
            do {
                let reply = try await api.send(command, secret: grant.secret)
                guard current(life), generation == controllerGeneration, sendToken == token, controllerControl?.controlEpoch == control.controlEpoch else { return }
                guard reply.queued, reply.controlEpoch == control.controlEpoch, reply.sequence == sequence else { throw CinemaRemoteOutcome.invalid }
                commandStatus = commandResult.known(epoch: control.controlEpoch, sequence: sequence)?.viewerMessage ?? "Sent. Waiting for the TV outcome."
            } catch {
                guard current(life), generation == controllerGeneration, sendToken == token, controllerControl?.controlEpoch == control.controlEpoch else { return }
                stopHolding()
                commandStatus = commandResult.known(epoch: control.controlEpoch, sequence: sequence)?.viewerMessage ?? "Command outcome unknown; it will not be replayed."
            }
            if generation == controllerGeneration && sendToken == token { sending = false }
        }
    }
    func beginHolding(_ direction: CinemaRemoteDirection) {
        stopHolding()
        send(.init(type: .navigate, direction: direction))
        holdTask = Task {
            do {
                try await Task.sleep(for: .milliseconds(350))
                while !Task.isCancelled, controlling {
                    send(.init(type: .navigate, direction: direction))
                    try await Task.sleep(for: .milliseconds(125))
                }
            } catch {}
        }
    }
    func stopHolding() { holdTask?.cancel(); holdTask = nil }
    var localGrants: [RemoteSecretStorage.Grant] { storage?.grants ?? [] }
    var localReceiverID: UUID? { receiver?.id ?? storage?.receiver?.id }
    func refreshGrants() async {
        guard let api else { managementStatus = "Enable Cinema remotes in Developer settings to manage pairings."; return }
        let generation = lifecycle
        do {
            let result = try await api.grants()
            guard current(generation) else { return }
            #if os(tvOS)
            pairedGrants = Array(result.grants.filter { $0.receiverID == localReceiverID }.prefix(20))
            #else
            pairedGrants = Array(result.grants.prefix(400))
            #endif
            managementStatus = ""
        } catch { if current(generation) { managementStatus = "Could not refresh paired phones." } }
    }
    func revokeGrant(_ id: UUID) async {
        guard let api else { return }
        let generation = lifecycle
        if selectedGrant?.id == id { closeController() }
        do {
            let result = try await api.revokeGrant(id)
            guard current(generation), result.revoked else { return }
            try storage?.forgetGrant(id)
            pairedGrants.removeAll { $0.id == id }
            if receiverControl?.activeGrantID == id { receiverControl = nil; guardState.deactivate() }
            managementStatus = "Pairing revoked."
        } catch { if current(generation) { managementStatus = "Revocation could not be confirmed. Retry while connected." } }
    }
    func forgetGrant(_ id: UUID) {
        if selectedGrant?.id == id { releaseControl(); closeController() }
        do { try storage?.forgetGrant(id); managementStatus = "Saved phone pairing removed. Server pairing remains until revoked." }
        catch { managementStatus = "Could not remove saved pairing." }
    }
    func resetReceiver() async {
        guard let api, let id = localReceiverID else { return }
        let generation = lifecycle
        do {
            let result = try await api.unregister(id)
            guard current(generation), result.revoked else { return }
            receiverTask?.cancel(); receiverTask = nil
            presenceTask?.cancel(); presenceTask = nil
            storage?.clearReceiver(); receiver = nil; target = nil; receiverControl = nil
            guardState.deactivate(); pairings = []; pairedGrants = []; challenge = nil
            managementStatus = "TV registration removed. Disable and re-enable Cinema remotes to register again."
        } catch { if current(generation) { managementStatus = "TV reset could not be confirmed. Retry while connected." } }
    }
    var suggestionDevices: [CinemaRemoteDevice] {
        devices.filter { device in device.available && device.target != nil && storage?.grants.contains(where: { $0.receiverID == device.id }) == true }
            .filter { !dismissedSuggestions.contains($0.id) && suggestionsEnabled(for: $0.id) }
    }
    func dismissSuggestion(_ id: UUID) { dismissedSuggestions.insert(id); objectWillChange.send() }
    func suggestionsEnabled(for id: UUID) -> Bool { UserDefaults.standard.object(forKey: "plurx.remote.suggest." + id.uuidString) as? Bool ?? true }
    func setSuggestions(_ value: Bool, for id: UUID) { UserDefaults.standard.set(value, forKey: "plurx.remote.suggest." + id.uuidString); objectWillChange.send() }
}
