import SwiftUI

struct SharedSharingSettingsView: View {
    @State private var summaries: [(String, String)] = []
    @State private var errors: [String] = []
    @State private var busy = false
    var body: some View {
        Form {
            Section("Libraries") { SharedLibrariesEntry() }
            Section("Enablement") {
                NavigationLink("Sharing enablement in Developer") { Form { SharedSharingDeveloperCard() }.navigationTitle("Sharing · Developer") }
                Text("Shared playback and physical-device qualification are still pending. Enablement remains your choice.").font(.caption)
            }
            Section("Server status") {
                ForEach(Array(summaries.enumerated()), id: \.offset) { _, row in LabeledContent(row.0, value: row.1) }
                ForEach(errors, id: \.self) { Text($0).foregroundStyle(.secondary) }
                Button("Refresh status") { Task { await load() } }.disabled(busy)
            }
            Section("Management") { NavigationLink("Manage invitations and Sources") { SharedSharingManagementView() } }
        }
        .navigationTitle("Sharing")
        .task { await load() }
    }
    private func load() async {
        busy = true; defer { busy = false }; summaries = []; errors = []
        do {
            let client = try SharedLibraryClient()
            for kind in ["status", "imports", "exports"] {
                do {
                    let object = try await client.management(kind); try client.requireCurrent()
                    if kind == "status" {
                        for key in ["listener", "serve", "outbound", "topology_qualification"] {
                            summaries.append((key.replacingOccurrences(of: "_", with: " ").capitalized, object[key]?.string ?? "Unknown"))
                        }
                        summaries.append(("Observation", "This server node"))
                    } else if case .array(let rows) = object[kind] {
                        for value in rows {
                            guard let entry = value.object else { continue }
                            let summary = entry[kind == "imports" ? "import" : "grant"]?.object ?? entry
                            summaries.append((kind == "imports" ? (summary["source_name"]?.string ?? "Shared Source") : "Export",
                                              summary["state"]?.string ?? "Unknown"))
                        }
                        if rows.isEmpty { summaries.append((kind.capitalized, "None")) }
                    }
                } catch { errors.append("\(kind.capitalized): \(error.localizedDescription)") }
            }
        } catch { errors.append(error.localizedDescription) }
    }
}

struct SharedSharingDeveloperCard: View {
    @State private var draft = SharedSharingDraft()
    @State private var busy = false
    @State private var saved: Bool?
    @State private var message = "Loading saved sharing choice…"
    var body: some View {
        Section("Shared libraries · advisory enablement") {
            Toggle("Enable Shared libraries", isOn: Binding(get: { draft.enabled }, set: { draft.choose($0) }))
                .accessibilityIdentifier("sharing-enabled-choice")
            if let saved { Text(saved ? "Saved choice: Enabled" : "Saved choice: Disabled").font(.caption) }
            Text("Source-labelled browsing is implemented. Shared playback and physical Apple TV/Google TV playback still await qualification.").font(.caption)
            Text("Unknown or unmet readiness never changes your selection or prevents Save.").font(.caption)
            Text("Leaves Developer when Shared playback, revocation, recovery and physical-device qualification pass; the permanent switch then moves to Settings → Sharing.").font(.caption)
            Text(message).font(.caption).accessibilityIdentifier("sharing-settings-status")
            Button("Save sharing choice") { Task { await save() } }.disabled(busy)
                .accessibilityIdentifier("sharing-enabled-save")
            Button("Reload saved choice") { Task { await load() } }.disabled(busy)
        }
        .task { await load() }
    }
    private func load() async {
        busy = true; defer { busy = false }
        let revision = draft.revision
        do {
            let client = try SharedLibraryClient(); let value = try await client.settings(); try client.requireCurrent()
            saved = value; draft.received(value, requestedAt: revision); message = "Saved choice loaded."
        } catch { message = "Saved choice unavailable: \(error.localizedDescription). Choose a value and Save to set it explicitly." }
    }
    private func save() async {
        busy = true; defer { busy = false }
        let value = draft.enabled; let revision = draft.revision
        do {
            let client = try SharedLibraryClient(); let result = try await client.save(enabled: value); try client.requireCurrent()
            saved = result; draft.received(result, requestedAt: revision); message = "Sharing choice saved."
        } catch { message = error.localizedDescription }
    }
}
