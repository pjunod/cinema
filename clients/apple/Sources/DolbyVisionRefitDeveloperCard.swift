import SwiftUI

/// Settings → Developer → Dolby Vision F+ curves: the four per-platform
/// switches (`playback.dolby_vision_refit_{web,apple,android,linux}`) and the
/// server's advisory readiness rows, as the web card draws them. With any
/// switch on, the server fits Profile 7 FEL titles in the background; copy
/// playback serves the curves only to the platforms switched on. Readiness
/// never disables a switch.
struct DolbyVisionRefitDeveloperCard: View {
    @EnvironmentObject private var model: AppModel
    @State private var readiness: DeveloperReadiness?
    @State private var busy = false
    @State private var message = "Loading saved F+ choices…"

    static let platforms: [(field: String, label: String)] = [
        ("dolby_vision_refit_web", "Web"),
        ("dolby_vision_refit_apple", "Apple"),
        ("dolby_vision_refit_android", "Android"),
        ("dolby_vision_refit_linux", "Linux (plurx Player)"),
    ]

    var body: some View {
        Section("Dolby Vision F+ curves · advisory readiness") {
            ForEach(Self.platforms, id: \.field) { platform in
                Toggle("Serve F+ curves to \(platform.label) clients", isOn: Binding(
                    get: { item(platform.field)?.enabled ?? false },
                    set: { on in Task { await save(platform.field, on) } }
                ))
                .disabled(busy || readiness == nil)
                .accessibilityIdentifier("\(platform.field)-choice")
            }
            Text("With any switch on, the server fits Profile 7 FEL titles in the background beside playback. A failed or cancelled title is fitted again only after its source changes or a retry. Readiness never prevents saving.").font(.caption)
            if let rows = item("dolby_vision_refit_apple")?.requirements {
                ForEach(rows) { requirement in
                    Label(requirement.title, systemImage: requirement.status == "met" ? "checkmark.circle" : (requirement.status == "unmet" ? "exclamationmark.triangle" : "questionmark.circle"))
                    Text(requirement.evidence).font(.caption)
                }
            }
            Text("Leaves Developer when copy-path serving of the curves and physical playback on each platform are recorded; the switches then move to Settings → Playback.").font(.caption)
            Text(message).font(.caption).accessibilityIdentifier("dv-refit-status")
            Button("Delete this node's statistics spool") { Task { await clearSpool() } }.disabled(busy)
            Button("Reload F+ readiness") { Task { await load() } }.disabled(busy)
        }
        .task { await load() }
    }

    private func item(_ id: String) -> DeveloperReadiness.Item? {
        readiness?.items.first(where: { $0.id == id })
    }

    private func load() async {
        busy = true; defer { busy = false }
        do {
            readiness = try await model.requireAPI().developerReadiness()
            message = "Saved choices loaded."
        } catch {
            message = "Saved choices unavailable: \(error.localizedDescription)"
        }
    }

    private func save(_ field: String, _ on: Bool) async {
        busy = true
        do {
            try await model.requireAPI().setDeveloperSwitch(field: field, enabled: on)
            busy = false
            await load()
            message = "F+ choice saved."
        } catch {
            busy = false
            message = error.localizedDescription
        }
    }

    private func clearSpool() async {
        busy = true; defer { busy = false }
        do {
            let removed = try await model.requireAPI().clearDolbyVisionRefitSpool()
            message = "Deleted \(removed) spool files on this node."
        } catch {
            message = error.localizedDescription
        }
    }
}
