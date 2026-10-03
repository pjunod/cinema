import SwiftUI

struct SharedSharingManagementView: View {
    @StateObject private var secrets = SharedSharingSecretDraft()
    @State private var libraries: [SharedSharingLocalLibrary] = []
    @State private var imports: [SharedSharingImport] = []
    @State private var exports: [SharedSharingExport] = []
    @State private var manifest: SharedSharingEndpointManifest?
    @State private var nextExport: String?
    @State private var exportCursors: Set<String> = []
    @State private var selected: Set<String> = []
    @State private var invitationId: String?
    @State private var busy = false
    @State private var message = ""
    @State private var confirmation: Confirmation?
    @State private var editingExport: SharedSharingExport?
    @State private var editingImport: SharedSharingImport?
    private enum Confirmation: Identifiable {
        case cancel(String), revoke(String), disconnect(String)
        var id: String { switch self { case .cancel(let id), .revoke(let id), .disconnect(let id): return id } }
        var label: String { switch self { case .cancel: return "Cancel invitation"; case .revoke: return "Revoke export"; case .disconnect: return "Disconnect Source" } }
    }
    var body: some View {
        Form {
            if secrets.snapshot() == nil { Text("This account changed. Leave and reopen Sharing management.") }
            else {
                Section("Source invitations") {
                    Text("Select this server's movie/show libraries. An invitation grants no access until you explicitly approve the recipient's pairing code.").font(.caption)
                    ForEach(libraries) { library in
                        Toggle(library.name, isOn: Binding(get: { selected.contains(library.sourceId) }, set: { value in
                            if value { selected.insert(library.sourceId) } else { selected.remove(library.sourceId) }; secrets.edit()
                        }))
                    }
                    Button("Create one-day invitation") { Task { await invite() } }.disabled(busy || selected.isEmpty)
                    TextField("Invitation to import or copy", text: Binding(get: { secrets.snapshot()?.invitation ?? "" }, set: { secrets.edit(invitation: $0) })).autocorrectionDisabled()
                    Text("Invitation material is transient and cleared on leaving or account changes.").font(.caption)
                    if let invitationId { Button("Cancel this invitation", role: .destructive) { confirmation = .cancel(invitationId) }.disabled(busy) }
                    Button("Import this invitation") { Task { await importSource() } }.disabled(busy || (secrets.snapshot()?.invitation.isEmpty ?? true))
                }
                Section("Imported Sources") {
                    ForEach(imports) { row in
                        VStack(alignment: .leading) {
                            Text(row.import.sourceName).font(.headline)
                            Text("\(row.import.sourceServerId) · \(row.import.catalogueEpoch)").font(.caption)
                            Text("\(row.import.state) · Pairing code: \(row.pairingCode)").font(.caption)
                            Text("Compare this code with the exporting server before approval.").font(.caption)
                            SharedSharingEndpointSummary(endpoints: row.import.endpoints)
                            Text("Lifecycle \(row.import.lifecycleGeneration) · Assignments \(row.import.assignmentGeneration) · Endpoints \(row.import.endpointGeneration)").font(.caption)
                            Button("Rotate credential") { Task { await rotate(row) } }.disabled(busy || row.import.state != "active")
                            Button("Re-pair using entered invitation") { Task { await rePair(row) } }.disabled(busy || (secrets.snapshot()?.invitation.isEmpty ?? true))
                            Button("Manage B viewer assignments") { editingImport = row }.disabled(busy || row.import.state != "active")
                            Button("Disconnect Source", role: .destructive) { confirmation = .disconnect(row.id) }.disabled(busy)
                        }
                    }
                    if imports.isEmpty { Text("No imported Sources.") }
                }
                Section("Exports") {
                    ForEach(exports) { row in
                        Button("\(row.recipientName) · \(row.grant.state)") { editingExport = row }
                        Button("Revoke \(row.recipientName)", role: .destructive) { confirmation = .revoke(row.id) }.disabled(busy)
                    }
                    if let nextExport { Button("Load more recipients") { Task { await loadMore(nextExport) } }.disabled(busy) }
                }
                Section("This server's endpoints") {
                    if let manifest { Text("Revision \(manifest.revision)"); SharedSharingEndpointSummary(endpoints: manifest.endpoints) }
                    else { Text("No configured endpoint manifest.") }
                    Text("Endpoint changes require a current generation and explicit pin confirmation. This screen currently shows the validated summary.").font(.caption)
                }
                Section { Button("Refresh management") { Task { await load() } }.disabled(busy); Text(message).font(.caption) }
            }
        }
        .navigationTitle("Sharing management")
        .sheet(item: $editingImport) { row in NavigationStack { SharedSharingAssignmentEditor(source: row.import) } }
        .sheet(item: $editingExport) { row in NavigationStack { SharedSharingExportEditor(row: row, libraries: libraries) } }
        .task { await load() }
        .onReceive(secrets.objectWillChange) { _ in
            if secrets.snapshot() == nil { imports = []; exports = []; invitationId = nil; editingExport = nil; editingImport = nil; confirmation = nil }
        }
        .onDisappear { secrets.leave(); imports = []; exports = []; invitationId = nil }
        .alert(confirmation?.label ?? "Confirm", isPresented: Binding(get: { confirmation != nil }, set: { if !$0 { confirmation = nil } })) {
            Button("Cancel", role: .cancel) { confirmation = nil }
            Button("Confirm", role: .destructive) { if let action = confirmation { confirmation = nil; Task { await confirmed(action) } } }
        } message: { Text("This changes access for the selected recipient or Source. It does not change sharing enablement.") }
    }
    private func load() async {
        guard secrets.snapshot() != nil else { return }; busy = true; defer { busy = false }
        do {
            let client = try SharedSharingManagementClient()
            let libs = try await client.libraries(); let imported = try await client.imports(); let page = try await client.exports(); let endpoints = try await client.endpoints()
            try client.requireCurrent(); guard secrets.snapshot() != nil else { return }
            libraries = libs; imports = imported; exports = page.rows; nextExport = page.next; exportCursors = []; manifest = endpoints; message = "Current management snapshot loaded."
        } catch { message = error.localizedDescription }
    }
    private func loadMore(_ cursor: String) async {
        guard !exportCursors.contains(cursor), secrets.snapshot() != nil else { return }; busy = true; defer { busy = false }
        do {
            let client = try SharedSharingManagementClient(); let page = try await client.exports(after: cursor); try client.requireCurrent()
            guard secrets.snapshot() != nil, nextExport == cursor else { return }
            exportCursors.insert(cursor); let known = Set(exports.map(\.id)); exports += page.rows.filter { !known.contains($0.id) }
            nextExport = page.next.flatMap { exportCursors.contains($0) ? nil : $0 }
        } catch { message = error.localizedDescription }
    }
    private func invite() async {
        guard let request = secrets.snapshot() else { return }; busy = true; defer { busy = false }
        let selection = Array(selected).sorted()
        do {
            let client = try SharedSharingManagementClient(); let result = try await client.invite(libraries: selection); try client.requireCurrent()
            invitationId = result.id
            guard secrets.accepts(request.revision) else { message = "Invitation created; a newer edit prevented replacing the draft. Refresh before another action."; return }
            secrets.edit(invitation: result.invitation); invitationId = result.id; message = "Invitation created. Compare the recipient pairing code before approving."
        } catch { message = error.localizedDescription }
    }
    private func importSource() async { await consumeInvitation { try await $0.importSource(invitation: $1) } }
    private func rePair(_ row: SharedSharingImport) async { await consumeInvitation { try await $0.rePair(row.import, invitation: $1) } }
    private func consumeInvitation(_ operation: (SharedSharingManagementClient, String) async throws -> SharedSharingImport) async {
        guard let request = secrets.snapshot() else { return }; busy = true; defer { busy = false }
        do {
            let client = try SharedSharingManagementClient(); let result = try await operation(client, request.invitation); try client.requireCurrent()
            guard secrets.snapshot() != nil else { return }
            imports.removeAll { $0.id == result.id }; imports.append(result); secrets.clear(ifRevision: request.revision)
            message = "Source updated. Compare the pairing code with the exporting server."
        } catch { message = "\(error.localizedDescription). Refresh current generations before retrying explicitly." }
    }
    private func rotate(_ row: SharedSharingImport) async {
        guard secrets.snapshot() != nil else { return }; busy = true; defer { busy = false }
        do { let client = try SharedSharingManagementClient(); let result = try await client.rotate(row.import); try client.requireCurrent(); guard secrets.snapshot() != nil else { return }; imports.removeAll { $0.id == result.id }; imports.append(result); message = "Credential rotation completed." }
        catch { message = error.localizedDescription }
    }
    private func confirmed(_ action: Confirmation) async {
        guard secrets.snapshot() != nil else { return }; busy = true; defer { busy = false }
        do {
            let client = try SharedSharingManagementClient()
            switch action { case .cancel(let id): try await client.cancelInvitation(id); case .revoke(let id): try await client.revoke(id); case .disconnect(let id): try await client.disconnect(id) }
            try client.requireCurrent(); guard secrets.snapshot() != nil else { return }
            message = "Access change saved. Refresh management to see the current state."
            if case .cancel = action, let revision = secrets.snapshot()?.revision { secrets.clear(ifRevision: revision); invitationId = nil }
        } catch { message = error.localizedDescription }
    }
}

private struct SharedSharingEndpointSummary: View {
    let endpoints: [SharedSharingEndpoint]
    var body: some View { ForEach(Array(endpoints.enumerated()), id: \.offset) { _, endpoint in
        Text("\(endpoint.tsFqdn):\(endpoint.port) · \(endpoint.ipv4)").font(.caption)
        Text("TLS pin: \(endpoint.spkiSha256)").font(.caption)
    } }
}

private struct SharedSharingExportEditor: View {
    @Environment(\.dismiss) private var dismiss
    let row: SharedSharingExport
    let libraries: [SharedSharingLocalLibrary]
    @StateObject private var secrets = SharedSharingSecretDraft()
    @State private var selected: Set<String>
    @State private var busy = false
    @State private var message = ""
    init(row: SharedSharingExport, libraries: [SharedSharingLocalLibrary]) { self.row = row; self.libraries = libraries; _selected = State(initialValue: Set(row.libraryIds)) }
    var body: some View {
        Form {
            if secrets.snapshot() == nil { Text("Account changed. Reopen this recipient.") }
            else {
                Text(row.recipientName); Text(row.grant.recipientServerId).font(.caption)
                Text("Mutation generation \(row.grant.mutationGeneration)").font(.caption)
                Section("Explicit pairing approval") {
                    Text("Read the code shown on the importing server and enter it here. Do not approve a recipient you did not intend to pair.").font(.caption)
                    TextField("Recipient's 16-character code", text: Binding(get: { secrets.snapshot()?.pairingCode ?? "" }, set: { secrets.edit(pairingCode: $0) })).autocorrectionDisabled()
                    Button("Approve entered code") { Task { await approve() } }.disabled(busy || row.grant.state != "pending")
                }
                Section("Export scope") {
                    ForEach(libraries) { library in Toggle(library.name, isOn: Binding(get: { selected.contains(library.sourceId) }, set: { value in if value { selected.insert(library.sourceId) } else { selected.remove(library.sourceId) }; secrets.edit() })) }
                    Button("Save selected scope") { Task { await scope() } }.disabled(busy)
                }
                Text(message).font(.caption)
            }
        }.navigationTitle("Sharing recipient").toolbar { Button("Done") { dismiss() } }.onDisappear { secrets.leave() }
    }
    private func approve() async {
        guard let draft = secrets.snapshot() else { return }; busy = true; defer { busy = false }
        do { let client = try SharedSharingManagementClient(); try await client.approve(row, enteredCode: draft.pairingCode); try client.requireCurrent(); guard secrets.snapshot() != nil else { return }; secrets.clear(ifRevision: draft.revision); message = "Approved. Return and refresh the recipient generation before further changes." }
        catch { message = error.localizedDescription }
    }
    private func scope() async {
        guard let draft = secrets.snapshot() else { return }; busy = true; defer { busy = false }; let selection = Array(selected).sorted()
        do { let client = try SharedSharingManagementClient(); try await client.scope(row, libraries: selection); try client.requireCurrent(); guard secrets.accepts(draft.revision) else { return }; message = "Scope saved. Return and refresh current generations before another change." }
        catch { message = error.localizedDescription }
    }
}

/// Whole-replacement editing requires complete authenticated admin Source and assignment reads.
private struct SharedSharingAssignmentEditor: View {
    @Environment(\.dismiss) private var dismiss
    let source: SharedSharingImportSummary
    @StateObject private var authority = SharedSharingSecretDraft()
    @State private var current: SharedSharingImportSummary?
    @State private var matrix: SharedSharingAssignmentMatrix?
    @State private var selectedLibrary = ""
    @State private var search = ""
    @State private var busy = false
    @State private var saved = false
    @State private var readCurrent = false
    @State private var message = ""
    private var visibleViewers: [SharedSharingViewer] {
        (matrix?.viewers ?? []).filter { search.isEmpty || $0.username.localizedCaseInsensitiveContains(search) || String($0.id).contains(search) }
    }
    var body: some View {
        Form {
            Text(source.sourceName).font(.headline)
            Text("\(source.sourceServerId) · \(source.catalogueEpoch)").font(.caption)
            if authority.snapshot() == nil { Text("Account changed. Close and reopen this Source.") }
            else if let matrix {
                Text("Assignment generation \(matrix.snapshot.expectedAssignmentGeneration)").font(.caption)
                Text("The complete matrix is retained. Libraries outside the current Source scope and unavailable viewers stay assigned until explicitly removed.").font(.caption)
                Picker("Source library", selection: $selectedLibrary) {
                    ForEach(matrix.libraries) { library in Text(library.name).tag(library.id) }
                }
                if let library = matrix.libraries.first(where: { $0.id == selectedLibrary }) {
                    Text("Source library ID: \(library.id)").font(.caption)
                    if library.outsideScope {
                        Button("Remove this outside-scope assignment group", role: .destructive) { edit { try $0.removeOutsideScope(library.id) } }.disabled(busy || saved)
                    }
                    TextField("Find B viewer by name or ID", text: $search).autocorrectionDisabled()
                    ForEach(Array(visibleViewers.prefix(100))) { viewer in
                        Toggle("\(viewer.username) · \(viewer.id)", isOn: Binding(get: { self.matrix?.contains(library: library.id, viewer: viewer.id) ?? false }, set: { enabled in edit { try $0.set(library: library.id, viewer: viewer.id, enabled: enabled) } })).disabled(busy || saved)
                    }
                    if visibleViewers.count > 100 { Text("Showing 100 of \(visibleViewers.count) matching viewers. Refine the search; hidden assignments remain in the complete matrix.").font(.caption) }
                }
                if matrix.libraries.isEmpty { Text("The Source has an explicitly empty current scope and no saved assignment groups.") }
                Button("Save complete viewer assignments") { Task { await save() } }.disabled(busy || saved || !readCurrent)
            } else { Text("No replacement matrix is available until all current reads succeed.") }
            Text(message).font(.caption)
            Button("Reload current matrix") { Task { await load() } }.disabled(busy)
        }
        .navigationTitle("Source viewers").toolbar { Button("Done") { authority.leave(); matrix = nil; dismiss() } }
        .task { await load() }.onDisappear { authority.leave(); matrix = nil }
        .onReceive(authority.objectWillChange) { _ in if authority.snapshot() == nil { matrix = nil; current = nil } }
    }
    private func edit(_ change: (inout SharedSharingAssignmentMatrix) throws -> Void) {
        guard authority.snapshot() != nil, !busy, !saved, var draft = matrix else { return }
        do { try change(&draft); matrix = draft; authority.edit() } catch { message = error.localizedDescription }
    }
    private func load() async {
        guard let requested = authority.snapshot(), !busy else { return }; busy = true; readCurrent = false; defer { busy = false }
        do {
            let client = try SharedSharingManagementClient()
            guard let row = try await client.imports().first(where: { $0.id == source.id })?.import,
                  row.sourceServerId == source.sourceServerId, row.catalogueEpoch == source.catalogueEpoch, row.state == "active" else { throw APIError.badURL }
            let assignments = try await client.assignments(row)
            let scope = try await client.sourceLibraries(row)
            let viewers = try await client.viewers()
            let result = try SharedSharingAssignmentMatrix(row: row, assignments: assignments, scope: scope, viewers: viewers)
            try client.requireCurrent(); guard authority.accepts(requested.revision) else { return }
            current = row; matrix = result; selectedLibrary = result.libraries.first?.id ?? ""; saved = false; readCurrent = true; message = "Complete current matrix loaded."
        } catch { message = "\(error.localizedDescription). Existing edits are retained; no empty matrix was inferred." }
    }
    private func save() async {
        guard let requested = authority.snapshot(), let current, let matrix, !busy, !saved, readCurrent else { return }
        busy = true; defer { busy = false }
        do {
            let client = try SharedSharingManagementClient()
            try await client.saveAssignments(matrix.snapshot, for: current, groups: matrix.groups)
            try client.requireCurrent(); guard authority.accepts(requested.revision), self.matrix?.accepts(matrix.revision) == true else { return }
            saved = true; message = "Assignments saved. Reload current generations before another change."
        } catch { message = "\(error.localizedDescription). No automatic retry; refresh and review before saving again." }
    }
}
