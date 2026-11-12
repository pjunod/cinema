import SwiftUI

/// Offload: Curator deleted this title's files to free disk. The item, its
/// metadata and its watch history stay, and Play is not offered — there is no
/// file. An administrator can ask Curator to download it again; everyone else
/// just sees where it stands. Nothing is stored here: the status line is read
/// from Curator while the page is open.
@MainActor
final class OffloadStatusModel: ObservableObject {
    @Published var line: String
    @Published var asking = false
    private var task: Task<Void, Never>?

    init(line: String = "Offloaded · the files were removed to free disk space") { self.line = line }

    static func statusLine(_ answer: OffloadAnswer) -> String { "Offloaded · \(answer.message)" }
    static let unreachable = "Offloaded · curator unreachable"

    func ask(itemId: Int, api: @escaping @MainActor () -> PlurxAPI) async {
        asking = true
        defer { asking = false }
        line = "Asking Curator…"
        do {
            let answer = try await api().getItBack(itemId: itemId)
            line = "Get it back: \(answer.message)"
            watch(itemId: itemId, api: api)
        } catch {
            line = error.localizedDescription
        }
    }

    /// Poll Curator's state while the page is on screen; `stop()` ends it.
    func watch(itemId: Int, api: @escaping @MainActor () -> PlurxAPI) {
        task?.cancel()
        task = Task { [weak self] in
            while !Task.isCancelled {
                do {
                    let answer = try await api().offloadStatus(itemId: itemId)
                    self?.line = Self.statusLine(answer)
                } catch {
                    if Task.isCancelled { return }
                    self?.line = Self.unreachable
                }
                try? await Task.sleep(nanoseconds: 5_000_000_000)
            }
        }
    }

    func stop() { task?.cancel(); task = nil }
}

struct OffloadBanner: View {
    @EnvironmentObject var model: AppModel
    let item: Item
    @StateObject private var status = OffloadStatusModel()

    @State private var isAdmin = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if isAdmin {
                Button {
                    Task { await status.ask(itemId: item.id, api: { model.requireAPI() }) }
                } label: {
                    Text(item.isOffloaded ? "Get it back" : "Get \(item.rollup?.offloaded ?? 0) back")
                }
                .disabled(status.asking)
                .accessibilityIdentifier("offload:get-it-back")
            }
            Text(status.line)
                .font(.callout)
                .foregroundStyle(Palette.muted)
                .accessibilityIdentifier("offload:status")
        }
        .task {
            isAdmin = (try? await model.requireAPI().me())?.isAdmin == true
            if item.isOffloaded { status.watch(itemId: item.id, api: { model.requireAPI() }) }
        }
        .onDisappear { status.stop() }
    }
}
