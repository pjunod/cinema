import SwiftUI
import CoreImage.CIFilterBuiltins

@MainActor
final class DiagnosticReportModel: ObservableObject {
    static let shared = DiagnosticReportModel()
    @Published var presented = false
    @Published var presenter = "root"
    @Published var draft: DiagnosticStore.Draft?
    @Published var pending: [DiagnosticStore.Draft] = []
    @Published private(set) var selectionRevision: UInt64 = 0
    @Published var message = ""
    @Published var busy = false
    @Published var destination = ""
    @Published var invitationCode = ""
    @Published var uploadConsent = false
    @Published var includeTitle = false
    @Published var includePlatform = false
    @Published var comparison: String?
    @Published var handoffState = ""
    @Published var shareURL: URL?
    @Published var history = DiagnosticRecorder.shared.historyEnabled
    private let store: DiagnosticStore
    private var workRevision: UInt64 = 0
    private var scope = "anonymous"
    private var generation: UInt64 = 0
    private var title: String?
    private var transport: DiagnosticTransport?
    private var operation: Task<Void, Never>?
    private var polling: Task<Void, Never>?
    private var authorizationObserver: UUID?
    var hasPendingUnsubmitted: Bool { (draft?.state != "submitted" && draft != nil) || pending.contains { $0.state != "submitted" } }
    init(store: DiagnosticStore = .shared) {
        self.store = store
        let installed = Session.shared.observeAuthorizationChanges { [weak self] _ in
            Task { @MainActor in self?.retire(); DiagnosticPlatform.shared.retire() }
        }
        authorizationObserver = installed.id
        DiagnosticPlatform.shared.start()
        DiagnosticRecorder.shared.record("lifecycle", "state", ["state": .text("starting")])
    }
    private func retire() {
        workRevision &+= 1
        operation?.cancel(); polling?.cancel(); transport?.cancel(); operation = nil; polling = nil; transport = nil
        draft = nil; pending = []; comparison = nil; handoffState = ""; presented = false; busy = false
        cleanupShare()
    }
    func configure(model: AppModel) async {
        guard model.phase != .loading else { return }
        let next = model.diagnosticNamespace; let auth = Session.shared.playbackAuthorization
        DiagnosticPlatform.shared.bind(next, generation: auth.generation)
        scope = next; generation = auth.generation
        await store.activate(next)
        await DiagnosticRecorder.shared.bind(namespace: next, generation: auth.generation, erasePrevious: false)
        guard Session.shared.playbackAuthorization.generation == auth.generation else { return }
        let restored = await store.drafts(next)
        guard Session.shared.playbackAuthorization.generation == auth.generation, scope == next else { return }
        pending = restored
    }
    func open(category: String = "other", title: String? = nil, owner: String = "root", newIncident: Bool = false) {
        workRevision &+= 1
        operation?.cancel(); polling?.cancel(); transport?.cancel(); cleanupShare()
        if newIncident { DiagnosticRecorder.shared.forgetReproduction() }
        self.title = title; includeTitle = false; includePlatform = false; comparison = nil; handoffState = ""; uploadConsent = false
        let capture = DiagnosticRecorder.shared.retainedReproduction ?? DiagnosticRecorder.shared.freeze()
        var value = DiagnosticStore.Draft(capture: capture, category: category, description: "", serverEvents: [], serverComponents: [], connection: [])
        if let previous = pending.first(where: { $0.id == capture.id }) { value = previous; value.capture = capture }
        let platform = DiagnosticPlatform.shared.projection(capture); value.platform = platform.events; value.platformReason = platform.reason
        presenter = owner; draft = value; presented = true; busy = true; message = "Capturing this incident. A description is optional."
        DiagnosticRecorder.shared.record("ui", "action", ["action": .text("report")])
        let client = DiagnosticTransport(scope: capture.namespace); transport = client
        operation = Task {
            var collected = value
            do {
                let projection = try await client.snapshot(capture)
                collected.serverEvents = projection.events ?? []; collected.serverComponents = projection.components ?? []
                if let n = UInt64(projection.dropped_events ?? "0"), n > 0,
                   let index = collected.serverComponents.firstIndex(where: { $0.name == "server" }) { collected.serverComponents[index].state = "partial"; collected.serverComponents[index].reason = "overflow" }
            } catch {
                collected.serverComponents = [DiagnosticReport.Component(name: "server", state: "unavailable", reason: capture.namespace == "anonymous" ? "context_unavailable" : "connection_failed")]
            }
            guard !Task.isCancelled, Session.shared.playbackAuthorization.generation == capture.generation, draft?.id == value.id else { return }
            if let origin = try? await client.supportOrigin() { destination = origin }
            guard !Task.isCancelled, draft?.id == value.id else { return }
            collected.description = draft?.description ?? ""; draft = collected
            await persist(collected); busy = false
            message = "Frozen local report. Review its coverage before sending. Missing server evidence does not prevent saving."
        }
    }
    func resume(_ row: DiagnosticStore.Draft) {
        guard !busy, row.capture.namespace == scope else { return }
        let previousWork = operation; let previousPoll = polling
        previousWork?.cancel(); previousPoll?.cancel(); transport?.cancel(); cleanupShare()
        workRevision &+= 1; let revision = workRevision
        let namespace = scope; let auth = Session.shared.playbackAuthorization
        var previous = draft
        if previous?.consented == false { previous?.title = includeTitle ? title : nil; previous?.includePlatform = includePlatform }
        busy = true; comparison = nil; handoffState = ""; transport = nil; polling = nil
        operation = Task {
            await previousWork?.value; await previousPoll?.value
            guard !Task.isCancelled, workRevision == revision, scope == namespace, Session.shared.playbackAuthorization.generation == auth.generation else { return }
            if let previous, !previous.consented { _ = await store.save(previous) }
            let rows = await store.drafts(namespace)
            guard !Task.isCancelled, workRevision == revision, scope == namespace, Session.shared.playbackAuthorization.generation == auth.generation else { return }
            pending = rows; busy = false
            guard var resumed = rows.first(where: { $0.id == row.id }) else { message = "This local report expired or was deleted."; return }
            // The row supplies identity only. The protected store supplies the
            // latest consent, immutable archive and support issuer together.
            resumed.capture = DiagnosticCapture(id: resumed.capture.id, namespace: namespace, generation: auth.generation,
                capturedAt: resumed.capture.capturedAt, incidentMs: resumed.capture.incidentMs, events: resumed.capture.events, dropped: resumed.capture.dropped,
                persistence: resumed.capture.persistence, historyEnabled: resumed.capture.historyEnabled, mode: resumed.capture.mode, windowMs: resumed.capture.windowMs)
            selectionRevision &+= 1
            draft = resumed; destination = resumed.origin ?? ""; uploadConsent = resumed.consented; includeTitle = resumed.title != nil
            title = resumed.title; includePlatform = resumed.includePlatform == true; presented = true; transport = DiagnosticTransport(scope: namespace)
            message = "Recovered local report. Save its diagnostic ZIP to attach it to an issue."
        }
    }
    private func persist(_ value: DiagnosticStore.Draft) async {
        _ = await store.save(value)
        let rows = await store.drafts(value.capture.namespace)
        guard !Task.isCancelled, scope == value.capture.namespace, Session.shared.playbackAuthorization.generation == value.capture.generation else { return }
        pending = rows
    }
    func close() {
        workRevision &+= 1
        if let draft, !draft.consented { Task { await store.save(draft) } }
        operation?.cancel(); polling?.cancel(); transport?.cancel(); transport = nil; busy = false; presented = false; cleanupShare()
    }
    func signOut() {
        let previous = scope
        retire()
        Task { await store.clear(previous) }
    }
    func suspend() {
        workRevision &+= 1
        operation?.cancel(); polling?.cancel(); transport?.cancel(); transport = nil; busy = false
        if let draft, !draft.consented { Task { await store.save(draft) } }
        if presented { message = "Paused in background. Retry the same consented report when foreground." }
    }
    func cleanupShare() { if let shareURL { try? FileManager.default.removeItem(at: shareURL.deletingLastPathComponent()) }; shareURL = nil }
    func setHistory(_ enabled: Bool) { history = enabled; Task { await DiagnosticRecorder.shared.setHistory(enabled) } }
    private func perform(_ action: @escaping (DiagnosticTransport, inout DiagnosticStore.Draft) async throws -> Void) {
        guard !busy, var value = draft, value.capture.generation == Session.shared.playbackAuthorization.generation else { return }; operation?.cancel(); transport?.cancel(); busy = true; message = "Working…"
        workRevision &+= 1; let revision = workRevision
        let client = DiagnosticTransport(scope: value.capture.namespace); transport = client
        operation = Task {
            do { try await action(client, &value); try client.current(); guard !Task.isCancelled, workRevision == revision, draft?.id == value.id else { return }; draft = value; await persist(value) }
            catch { if !Task.isCancelled, workRevision == revision, draft?.id == value.id { draft = value; await persist(value); message = (error as? LocalizedError)?.errorDescription ?? "The connection failed. Retry or save this local report." } }
            if !Task.isCancelled, workRevision == revision { busy = false }
        }
    }
    private func archive(_ value: inout DiagnosticStore.Draft) async throws -> DiagnosticReport.Archive {
        if let binding = value.binding, let manifest = value.manifest, let bytes = await store.archive(value.id, scope: value.capture.namespace) {
            guard bytes.count == binding.length, DiagnosticRecorder.digest(bytes) == binding.sha256 else { throw DiagnosticTransport.Failure.invalid }
            return DiagnosticReport.Archive(bytes: bytes, binding: binding, manifest: manifest)
        }
        guard value.binding == nil else { throw DiagnosticTransport.Failure.storage }
        value.title = includeTitle ? title : nil; value.includePlatform = includePlatform
        let snapshot = value; let client = DiagnosticReport.client
        let worker = Task.detached(priority: .utility) { try DiagnosticReport.archive(snapshot, client: client) }
        let result = try await withTaskCancellationHandler(operation: { try await worker.value }, onCancel: { worker.cancel() })
        try Task.checkCancellation()
        guard result.bytes.count <= 8_388_608 else { throw DiagnosticTransport.Failure.limit }
        if value.consented {
            value.binding = result.binding; value.manifest = result.manifest
            let persistence = await store.saveArchive(result.bytes, id: value.id, scope: value.capture.namespace, generation: value.capture.generation)
            guard persistence != "unavailable" else { throw DiagnosticTransport.Failure.storage }
            await persist(value)
        }
        return result
    }
    func export() {
        perform { [weak self] _, value in
            guard let self else { return }; let report = try await self.archive(&value)
            let id = UUID().uuidString.lowercased(); let bytes = report.bytes; let reportID = value.id
            let worker = Task.detached(priority: .utility) { () throws -> URL in
                let folder = FileManager.default.temporaryDirectory.appendingPathComponent("cinema-share-" + id, isDirectory: true)
                try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
                let url = folder.appendingPathComponent("cinema-report-" + reportID + ".zip")
                try bytes.write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication]); return url
            }
            let url = try await worker.value; guard !Task.isCancelled else { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()); return }
            self.shareURL = url; self.message = "Diagnostic ZIP ready. Attach it to your bug or issue report."
        }
    }
    func send(phone: Bool = false) {
        guard !DiagnosticRecorder.shared.recording else { message = "Stop reproduction before sending this frozen report."; return }
        guard uploadConsent else { message = DiagnosticTransport.Failure.consent.errorDescription!; return }
        let origin: String
        do { origin = try DiagnosticTransport.origin(destination) } catch { message = "Choose a fixed HTTPS support destination."; return }
        perform { [weak self] client, value in
            guard let self else { return }
            if let prior = value.origin, value.consented, prior != origin { throw DiagnosticTransport.Failure.invalid }
            value.origin = origin; value.consented = true
            let report = try await self.archive(&value); try await client.stage(report, draft: &value)
            if phone && value.state != "submitted" { try await client.offer(&value); self.message = "Scan on the phone, then compare its phrase here before approving." }
            else { try await client.submit(&value); DiagnosticRecorder.shared.forgetReproduction(value.id); self.message = "Submitted. Reference: " + (value.reference ?? "unavailable") }
            self.draft = value; if value.handoff != nil { self.pollHandoff() }
        }
    }
    func redeemInvitation() {
        let code = invitationCode.trimmingCharacters(in: .whitespacesAndNewlines).uppercased(); let destination = destination
        guard !DiagnosticRecorder.shared.recording else { message = "Stop reproduction before sending this frozen report."; return }
        guard uploadConsent else { message = DiagnosticTransport.Failure.consent.errorDescription!; return }
        perform { [weak self] client, value in
            let origin = try DiagnosticTransport.origin(destination)
            try await client.invitation(origin: origin, code: code, reportID: value.id)
            value.origin = origin; self?.invitationCode = ""; self?.message = "One-report invitation ready. Send only after reviewing this capture."
        }
    }
    func pollHandoff() {
        polling?.cancel(); guard let value = draft, value.handoff != nil else { return }
        let client = DiagnosticTransport(scope: value.capture.namespace); let id = value.id
        polling = Task {
            defer { client.cancel() }; var deadline = ContinuousClock.now.advanced(by: .seconds(600)); var approved = false
            while !Task.isCancelled, ContinuousClock.now < deadline, draft?.id == id, presented {
                do {
                    let status = try await client.handoffStatus(value); try client.current()
                    guard !Task.isCancelled, draft?.id == id, presented else { return }; handoffState = status.state
                    comparison = status.state == "claim_pending" ? status.comparison : nil
                    if status.state == "approved", !approved { approved = true; deadline = ContinuousClock.now.advanced(by: .seconds(1800)); message = "Phone approved. It can finish this report even if the TV closes." }
                    if status.state == "submitted" { DiagnosticRecorder.shared.forgetReproduction(id); draft?.state = "submitted"; draft?.reference = status.reference; message = "Submitted. Reference: " + (status.reference ?? "unavailable"); if let draft { await persist(draft) }; return }
                    if ["expired","cancelled","rejected","deleted"].contains(status.state) { message = "Phone handoff ended. The local report is still available."; return }
                } catch { if !Task.isCancelled { message = "Handoff status unavailable. Retry when foreground." }; return }
                try? await Task.sleep(for: .seconds(2))
            }
        }
    }
    /// This button exists only on the physically controlled, remotely blocked
    /// report sheet. No companion action can select or approve it.
    func approve() {
        guard let phrase = comparison else { return }
        perform { [weak self] client, value in try await client.approve(value, comparison: phrase); self?.comparison = nil; self?.message = "Phone approved for this report only." }
    }
    func discard(keep: Bool = false) {
        perform { [weak self] client, value in
            try await client.delete(value, keep: keep)
            if !keep { DiagnosticRecorder.shared.forgetReproduction(value.id); await self?.store.delete(value.id, scope: value.capture.namespace); self?.pending.removeAll { $0.id == value.id }; self?.draft = nil; self?.presented = false; self?.busy = false; self?.message = "Draft deleted." }
            else { value.handoff = nil; self?.message = "Phone handoff cancelled; staged draft retained." }
        }
    }
    func startReproduction() {
        guard let capture = draft?.capture, draft?.binding == nil else { return }
        if DiagnosticRecorder.shared.startReproduction(capture) { message = "Recording for up to five minutes. Reproduce the issue, then reopen Help and stop. Recording ends on background." }
    }
    func systemSnapshot() {
        perform { [weak self] client, value in
            guard value.binding == nil else { throw DiagnosticTransport.Failure.invalid }
            let result = try await client.snapshot(value.capture, includeSystem: true)
            value.serverEvents = result.events ?? []; value.serverComponents = result.components ?? []
            self?.message = "Administrator snapshot saved. Only typed version, readiness, producer and hardware admission facts are included."
        }
    }
    func checks() {
        perform { [weak self] client, value in
            guard value.binding == nil else { throw DiagnosticTransport.Failure.invalid }
            value.connection += try await client.connectionChecks()
            if value.connection.count > 100 { value.connection = Array(value.connection.suffix(100)) }
            self?.message = "Checks saved alongside the original incident. No media transfer was requested; unavailable phases are explicit."
        }
    }
    func stopReproduction() {
        if let capture = DiagnosticRecorder.shared.stopReproduction() { draft?.capture = capture; message = "Reproduction stopped and frozen locally." }
    }
    func deleteLocal(_ row: DiagnosticStore.Draft) {
        guard row.capture.namespace == scope else { return }
        let selected = draft?.id == row.id
        let previousWork = selected ? operation : nil; let previousPoll = selected ? polling : nil
        if selected {
            workRevision &+= 1; previousWork?.cancel(); previousPoll?.cancel(); transport?.cancel()
            operation = nil; polling = nil; transport = nil; draft = nil; busy = false
            comparison = nil; handoffState = ""; cleanupShare()
        }
        pending.removeAll { $0.id == row.id }; DiagnosticRecorder.shared.forgetReproduction(row.id)
        let namespace = scope; let auth = Session.shared.playbackAuthorization
        Task {
            await store.delete(row.id, scope: namespace)
            await previousWork?.value; await previousPoll?.value
            await store.delete(row.id, scope: namespace)
            let rows = await store.drafts(namespace)
            guard scope == namespace, Session.shared.playbackAuthorization.generation == auth.generation else { return }
            pending = rows; if selected { message = "Local report deleted." }
        }
    }
}

struct DiagnosticReportButton: View {
    var category = "other"
    var title: String?
    var owner = "root"
    var body: some View { Button("Report a problem") { DiagnosticReportModel.shared.open(category: category, title: title, owner: owner) }.accessibilityIdentifier("diagnostics-open") }
}

struct DiagnosticReportView: View {
    @ObservedObject var reports = DiagnosticReportModel.shared
    @EnvironmentObject private var remoteNavigation: RemoteNavigationCoordinator
    @State private var block = UUID()
    var body: some View {
        NavigationStack {
            Form {
                incidentSection
                pendingSection
            }
            // Each selected incident starts at its coverage and privacy
            // summary, instead of retaining another incident's scroll offset.
            .id((reports.draft?.id ?? "") + "-" + String(reports.selectionRevision))
            #if os(tvOS)
            .pickerStyle(.menu)
            #endif
            .navigationTitle("Report a problem")
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Close") { reports.close() } } }
        }
        .remoteRestricted()
        .onAppear { remoteNavigation.setBlocked(block, true) }
        .onDisappear {
            // Presenting the system share sheet must not retire the report or
            // delete the ZIP while the system still owns its file URL.
            if reports.shareURL == nil { remoteNavigation.setBlocked(block, false); reports.close() }
        }
        #if os(iOS)
        .sheet(item: Binding(get: { reports.shareURL.map { DiagnosticShareItem(url: $0) } }, set: { if $0 == nil { reports.cleanupShare() } })) { item in
            DiagnosticShareView(url: item.url)
        }
        #endif
    }
    private var incidentSection: some View {
        Section("Incident report") {
                    Button("Capture a new incident") { reports.open(owner: reports.presenter, newIncident: true) }.disabled(reports.busy || DiagnosticRecorder.shared.recording)
                    Text(reports.message).accessibilityIdentifier("diagnostics-status")
                    if reports.busy { ProgressView() }
                    if let draft = reports.draft {
                        incidentDescription(draft)
                        incidentPrivacy(draft)
                        incidentActions(draft)
                    }
                }
    }
    private func incidentDescription(_ draft: DiagnosticStore.Draft) -> some View {
        Group {
            Text("Report " + draft.id).font(.caption)
                        Picker("Category", selection: Binding(get: { reports.draft?.category ?? "other" }, set: { reports.draft?.category = $0 })) {
                            Text("Playback").tag("playback"); Text("Connection / sign-in").tag("connection"); Text("Interface / interrupted run").tag("interface"); Text("Other").tag("other")
                        }.disabled(draft.consented)
                        #if os(iOS)
                        TextEditor(text: Binding(get: { reports.draft?.description ?? "" }, set: { reports.draft?.description = String($0.prefix(4_000)) })).frame(minHeight: 100).accessibilityLabel("What happened? Description is user content")
                            .disabled(draft.consented)
                        #else
                        TextField("Optional description", text: Binding(get: { reports.draft?.description ?? "" }, set: { reports.draft?.description = String($0.prefix(4_000)) })).disabled(draft.consented)
                        #endif
        }
    }
    private func incidentPrivacy(_ draft: DiagnosticStore.Draft) -> some View {
        Group {
            Toggle("Include the current title", isOn: $reports.includeTitle).disabled(draft.consented)
                        Toggle("Include available platform diagnostics", isOn: $reports.includePlatform).disabled(draft.consented || draft.platform?.isEmpty != false)
                        Text("No passwords, addresses, request bodies, media or raw stream identifiers are collected. Your description and an optional title are private content.").font(.caption)
                        Text("Client: " + (draft.capture.historyEnabled ? (draft.capture.dropped > 0 ? "partial (overflow)" : "bounded history") : "history disabled") + "; persistence: " + draft.capture.persistence).font(.caption)
                        ForEach(draft.serverComponents, id: \.name) { component in Text("\(component.name): \(component.state) \(component.reason ?? "")").font(.caption) }
                        Text("Platform diagnostics: " + (draft.platformReason ?? "normalized evidence available") + ". Clock alignment: unavailable.").font(.caption)
                        #if os(tvOS)
                        Text("Recent safe events").font(.headline)
                        eventPreview(draft)
                        #else
                        DisclosureGroup("Preview safe event fields") { eventPreview(draft) }
                        #endif
        }
    }
    private func eventPreview(_ draft: DiagnosticStore.Draft) -> some View {
        ForEach(Array(draft.capture.events.suffix(30).enumerated()), id: \.offset) { _, event in
            Text(event.utc + " " + event.domain + "/" + event.kind + " " + event.facts.keys.sorted().joined(separator: ", ")).font(.caption)
        }
    }
    private func incidentActions(_ draft: DiagnosticStore.Draft) -> some View {
        Group {
            #if os(iOS)
                        Button("Save / share diagnostic ZIP") { reports.export() }.disabled(reports.busy).accessibilityIdentifier("diagnostics-export")
                        Text("Attach the ZIP to your bug or issue report. Saving does not submit it.").font(.caption)
                        #else
                        Text("This TV keeps the report locally. Transferring the TV's ZIP to a phone is not available yet.").font(.caption)
                        #endif
                        if DiagnosticRecorder.shared.recording { Button("Stop reproduction recording") { reports.stopReproduction() } }
                        else { Button("Record reproduction (five minutes)") { reports.startReproduction() }.disabled(draft.consented || reports.busy) }
                        Button("Check this Cinema connection (15 seconds)") { reports.checks() }.disabled(draft.consented || reports.busy)
                        Button("Collect administrator system snapshot") { reports.systemSnapshot() }.disabled(draft.consented || reports.busy)
                        Text("Requires a Cinema administrator account.").font(.caption)
                        Text("Checks contact only the configured Cinema endpoint, with a total 2 MiB reply ceiling. No media or cellular transfer probe is started.").font(.caption)
        }
    }
    private var pendingSection: some View {
        Section("Local history and pending reports") {
                    Toggle("Keep recent diagnostic history", isOn: Binding(get: { reports.history }, set: { reports.setHistory($0) }))
                    Text("Local captures expire within 24 hours. Closing keeps this draft. Clearing local data does not delete an already submitted support report.").font(.caption)
                    ForEach(reports.pending) { value in
                        Button("Resume \(value.capture.capturedAt) · \(value.state)") { reports.resume(value) }.disabled(reports.busy).accessibilityIdentifier("diagnostics-resume-" + value.id)
                        Button("Delete local \(value.capture.capturedAt)", role: .destructive) { reports.deleteLocal(value) }
                    }
                }
    }

}

struct DiagnosticHandoffView: View {
    let draft: DiagnosticStore.Draft
    @ObservedObject var reports = DiagnosticReportModel.shared
    private var qr: UIImage? {
        guard let text = draft.handoff?.url, text.utf8.count <= 1024 else { return nil }
        let filter = CIFilter.qrCodeGenerator(); filter.message = Data(text.utf8)
        guard let output = filter.outputImage?.transformed(by: CGAffineTransform(scaleX: 8, y: 8)), let image = CIContext().createCGImage(output, from: output.extent) else { return nil }
        return UIImage(cgImage: image)
    }
    var body: some View {
        if let offer = draft.handoff {
            if let qr { Image(uiImage: qr).interpolation(.none).resizable().scaledToFit().frame(width: 256, height: 256).accessibilityLabel("Scan to claim this report on your phone") }
            Text("Open \(draft.origin ?? "support") on your phone. Manual code: \(offer.manual_code)").font(.headline)
            Text("Claim expires in ten minutes. Keep this screen open and compare the phrase on both devices.")
            Text("Handoff: " + reports.handoffState)
            if let phrase = reports.comparison {
                Text("Compare on phone: " + phrase).font(.title)
                Button("The phrase matches — approve phone") { reports.approve() }.disabled(reports.busy).accessibilityIdentifier("diagnostics-approve-phone")
            }
            Button("Refresh handoff status") { reports.pollHandoff() }.disabled(reports.busy)
        }
    }
}

#if os(iOS)
private struct DiagnosticShareItem: Identifiable {
    let url: URL
    var id: String { url.path }
}

struct DiagnosticShareView: UIViewControllerRepresentable {
    let url: URL
    func makeUIViewController(context: Context) -> UIActivityViewController {
        let view = UIActivityViewController(activityItems: [url], applicationActivities: nil)
        // SwiftUI owns the sheet presentation, including the iPad anchor.
        view.completionWithItemsHandler = { _, _, _, _ in Task { @MainActor in DiagnosticReportModel.shared.cleanupShare() } }
        return view
    }
    func updateUIViewController(_ uiViewController: UIActivityViewController, context: Context) {}
}
#endif

private struct DiagnosticPresentation: ViewModifier {
    let owner: String
    @ObservedObject var reports = DiagnosticReportModel.shared
    func body(content: Content) -> some View {
        content.sheet(isPresented: Binding(get: { reports.presented && reports.presenter == owner }, set: { if !$0 { reports.close() } })) { DiagnosticReportView() }
    }
}
extension View {
    func diagnosticReportHost(owner: String = "root") -> some View { modifier(DiagnosticPresentation(owner: owner)) }
}
