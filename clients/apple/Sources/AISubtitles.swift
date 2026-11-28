import Foundation
import SwiftUI

struct AISubtitleOptions: Decodable {
    struct Choice: Decodable {
        let route: String
        let label: String
    }
    let target: String?
    let allowed: Bool
    let label: String?
    let route: String?
    let alternatives: [Choice]
    let existingIndex: Int?
    let searchOnline: Bool?
    let operation: AISubtitleOperation?

    var canStart: Bool { allowed && existingIndex == nil && route != nil }
}

struct AISubtitleOperation: Decodable {
    let id: String
    let state: String
    let progress: Double
    let trackId: String?
    let trackIndex: Int?
    let route: String?

    var active: Bool { state == "queued" || state == "working" }
}

struct AISubtitleWindow: Decodable {
    struct Cue: Decodable {
        let startMs: Int
        let endMs: Int
        let text: String
    }
    let state: String
    let trackId: String?
    let readyFromMs: Int?
    let readyToMs: Int
    let cues: [Cue]
}

struct AISubtitlePreferences: Decodable {
    var language: String
    var mode: String

    var body: [String: CaptionValue] { ["language": .string(language), "mode": .string(mode)] }
}

struct AISubtitleSettings: Decodable {
    struct Readiness: Decodable {
        struct Node: Decodable, Identifiable {
            let nodeId: String
            let label: String
            let state: String
            let testedAtMs: Int?
            var id: String { nodeId }
        }
        struct Summary: Decodable { let ready: Int; let nodes: Int }
        let summary: Summary
        let nodes: [Node]
    }
    let version: Int
    let revision: String
    var downloadMatch: Bool
    var translateText: Bool
    var fromAudio: Bool
    var aheadOfTime: Bool
    var who: String
    var quality: String
    var readiness: Readiness?

    var body: [String: CaptionValue] {
        ["version": .integer(version), "expected_revision": .string(revision),
         "download_match": .bool(downloadMatch), "translate_text": .bool(translateText),
         "from_audio": .bool(fromAudio), "ahead_of_time": .bool(aheadOfTime),
         "who": .string(who), "quality": .string(quality)]
    }
}

struct AISubtitleSettingsView: View {
    @EnvironmentObject private var model: AppModel
    @State private var preferences: AISubtitlePreferences?
    @State private var settings: AISubtitleSettings?
    @State private var status = ""
    @State private var isAdmin = false
    @State private var saving = false
    @State private var showNodes = false

    private var api: PlurxAPI {
        model.requireAPI().captionOwner(generation: Session.shared.playbackAuthorization.generation)
    }

    var body: some View {
        Form {
            if let preferences = Binding($preferences) {
                Section("Defaults") {
                    let current = preferences.language.wrappedValue
                    SettingsPicker("Subtitle language", value: Locale.current.localizedString(forLanguageCode: current) ?? current,
                                   selection: preferences.language) {
                        ForEach(Array(Set([current, "eng", "deu", "spa", "fra", "ita", "por", "jpn", "kor",
                                           "zho", "ara", "hin", "rus", "tur", "vie", "nld"])).sorted(), id: \.self) { code in
                            Text(Locale.current.localizedString(forLanguageCode: code) ?? code).tag(code)
                        }
                    }
                    Picker("Show subtitles", selection: preferences.mode) {
                        Text("Auto · only for foreign audio").tag("auto")
                        Text("Always").tag("always")
                        Text("Off").tag("off")
                    }
                    Button("Save defaults") { Task { await savePreferences() } }.disabled(saving)
                }
            }
            if isAdmin, let settings = Binding($settings) {
                Section("When a title has no English subtitles") {
                    Toggle("Download a match", isOn: settings.downloadMatch)
                    Toggle("Translate its other subtitles", isOn: settings.translateText)
                    Toggle("Create them from the audio", isOn: settings.fromAudio)
                    Toggle("Also ahead of time for new titles", isOn: settings.aheadOfTime)
                    Picker("Who can start AI subtitles", selection: settings.who) {
                        Text("Everyone").tag("everyone")
                        Text("Admins only").tag("admins")
                    }
                }
                Section("AI quality") {
                    Picker("Quality", selection: settings.quality) {
                        Text("Fast").tag("fast")
                        Text("Balanced").tag("balanced")
                        Text("Best").tag("best")
                    }
                    Button("Save server settings") { Task { await saveSettings() } }.disabled(saving)
                    if let readiness = settings.wrappedValue.readiness {
                        Button("Ready on \(readiness.summary.ready) of \(readiness.summary.nodes) servers") {
                            showNodes.toggle()
                        }
                        if showNodes {
                            ForEach(readiness.nodes) { node in
                                LabeledContent(node.label, value: node.state.capitalized)
                            }
                        }
                    }
                    Button("Refresh status") { Task { await load() } }.disabled(saving)
                }
            }
            if !status.isEmpty { Text(status) }
        }
        .navigationTitle("Subtitles")
        .task { await load() }
    }

    @MainActor private func load() async {
        do {
            let prefs: CaptionHTTPReply<AISubtitlePreferences> = try await api.caption("ai-subtitles/preferences")
            preferences = prefs.value
            let account = try await api.me()
            isAdmin = account.isAdmin == true
            if isAdmin {
                let reply: CaptionHTTPReply<AISubtitleSettings> = try await api.caption("ai-subtitles/settings")
                settings = reply.value
            }
        } catch { if !Task.isCancelled { status = error.localizedDescription } }
    }

    @MainActor private func savePreferences() async {
        guard let preferences else { return }
        saving = true
        defer { saving = false }
        do {
            let reply: CaptionHTTPReply<AISubtitlePreferences> = try await api.caption(
                "ai-subtitles/preferences", method: "PUT", body: preferences.body)
            self.preferences = reply.value
            model.setLanguages(audio: model.audioLang, sub: reply.value.mode == "off" ? "off" : reply.value.language)
            status = "Subtitle defaults saved."
        } catch { status = error.localizedDescription }
    }

    @MainActor private func saveSettings() async {
        guard let settings else { return }
        saving = true
        defer { saving = false }
        do {
            let _: CaptionHTTPReply<AISubtitleSettings> = try await api.caption(
                "ai-subtitles/settings", method: "PUT", body: settings.body)
            status = "AI subtitle settings saved."
            await load()
        } catch { status = error.localizedDescription }
    }
}

struct AISubtitleItemView: View {
    @EnvironmentObject private var model: AppModel
    let file: MediaFile
    let audioIndex: Int?
    let changed: (_ trackId: String?, _ index: Int?) async -> Void
    @State private var options: AISubtitleOptions?
    @State private var operation: AISubtitleOperation?
    @State private var status = ""
    @State private var busy = false
    @State private var matches: [OnlineMatch] = []

    private struct OnlineMatch: Decodable, Identifiable {
        let fileId: Int
        let release: String
        let language: String
        var id: Int { fileId }
    }
    private struct Search: Decodable { let results: [OnlineMatch] }
    private struct Download: Decodable { let subtitleIndex: Int }
    private struct Timing: Decodable { let offsetMs: Int }

    private var path: String { "files/\(file.id)/ai-subtitles" }
    private var language: String { file.playbackDefaults?.subtitle?.preferredLanguage ?? "en" }
    private var api: PlurxAPI {
        model.requireAPI().captionOwner(generation: Session.shared.playbackAuthorization.generation)
    }

    var body: some View {
        Form {
            if let options, options.canStart {
                Button(options.label ?? "AI subtitles") { Task { await start(route: "auto") } }
                    .disabled(busy || operation?.active == true)
                ForEach(options.alternatives, id: \.route) { choice in
                    Button(choice.label) { Task { await start(route: choice.route) } }
                        .disabled(busy || operation?.active == true)
                }
            }
            if options?.searchOnline == true {
                Button("Search online") { Task { await search() } }.disabled(busy)
                ForEach(matches) { match in
                    Button(match.release) { Task { await download(match) } }.disabled(busy)
                }
            }
            ForEach((file.subtitleStreams ?? []).filter { $0.ai == true }) { track in
                Section("English · AI") {
                    Button("Redo at Best quality") { Task { await start(route: "audio", quality: "best") } }
                        .disabled(busy || operation?.active == true)
                    Button("Earlier by 250 ms") { Task { await shift(track, by: -250) } }.disabled(busy)
                    Button("Later by 250 ms") { Task { await shift(track, by: 250) } }.disabled(busy)
                    Button("Delete", role: .destructive) { Task { await remove(track) } }.disabled(busy)
                }
            }
            if let operation, operation.active {
                ProgressView("English · AI", value: operation.progress)
                Text("You can play this title while subtitles are prepared.")
            }
            if !status.isEmpty { Text(status) }
        }
        .navigationTitle("Subtitles")
        .task { await load() }
        .task(id: operation?.id) {
            while operation?.active == true && !Task.isCancelled {
                try? await Task.sleep(for: .seconds(3))
                if Task.isCancelled { return }
                await poll()
            }
        }
    }

    @MainActor private func poll() async {
        guard let id = operation?.id else { return }
        do {
            let reply: CaptionHTTPReply<AISubtitleOperation> = try await api.caption("ai-subtitles/operations/\(id)")
            guard !Task.isCancelled, operation?.id == id, reply.value.id == id else { return }
            operation = reply.value
            if reply.value.state == "complete" {
                status = "English AI subtitles are ready."
                await changed(reply.value.trackId, reply.value.trackIndex)
            } else if !reply.value.active {
                status = "AI subtitles could not finish."
            }
        } catch {
            if !Task.isCancelled { status = error.localizedDescription }
        }
    }

    @MainActor private func load() async {
        do {
            var query = [URLQueryItem(name: "target", value: language)]
            if let audioIndex { query.append(URLQueryItem(name: "audio_index", value: String(audioIndex))) }
            let reply: CaptionHTTPReply<AISubtitleOptions> = try await api.caption(path, query: query)
            options = reply.value
            if operation == nil { operation = reply.value.operation }
        } catch { if !Task.isCancelled { status = error.localizedDescription } }
    }

    @MainActor private func start(route: String, quality: String? = nil) async {
        busy = true
        defer { busy = false }
        var body: [String: CaptionValue] = ["request_id": .string(UUID().uuidString.lowercased()),
            "target": .string("en"), "route": .string(route)]
        if let quality { body["quality"] = .string(quality) }
        if let audioIndex { body["audio_index"] = .integer(audioIndex) }
        do {
            let reply: CaptionHTTPReply<AISubtitleOperation> = try await api.caption(path, method: "POST", body: body)
            operation = reply.value
            if reply.value.state == "complete" { await changed(reply.value.trackId, reply.value.trackIndex) }
        } catch { status = error.localizedDescription }
    }

    @MainActor private func search() async {
        busy = true
        defer { busy = false }
        do {
            let reply: CaptionHTTPReply<Search> = try await api.caption("files/\(file.id)/subtitles/search",
                query: [URLQueryItem(name: "language", value: options?.target ?? language)])
            matches = reply.value.results
            status = matches.isEmpty ? "No subtitle matches found." : ""
        } catch { status = error.localizedDescription }
    }

    @MainActor private func download(_ match: OnlineMatch) async {
        busy = true
        defer { busy = false }
        do {
            let reply: CaptionHTTPReply<Download> = try await api.caption("files/\(file.id)/subtitles/download", method: "POST",
                body: ["provider_file_id": .integer(match.id), "language": .string(match.language)])
            status = "Subtitle downloaded."
            await changed(nil, reply.value.subtitleIndex)
        } catch { status = error.localizedDescription }
    }

    @MainActor private func shift(_ track: SubtitleStream, by delta: Int) async {
        guard let id = track.trackId else { return }
        busy = true
        defer { busy = false }
        do {
            let path = "files/\(file.id)/subtitles/\(track.index)/timing"
            let query = [URLQueryItem(name: "track_id", value: id)]
            let current: CaptionHTTPReply<Timing> = try await api.caption(path, query: query)
            let offset = min(15_000, max(-15_000, current.value.offsetMs + delta))
            let saved: CaptionHTTPReply<Timing> = try await api.caption(path, method: "PUT",
                body: ["offset_ms": .integer(offset)], query: query)
            status = "Subtitle timing \(saved.value.offsetMs) ms"
        } catch { status = error.localizedDescription }
    }

    @MainActor private func remove(_ track: SubtitleStream) async {
        guard let id = track.trackId else { return }
        busy = true
        defer { busy = false }
        do {
            _ = try await api.captionBytes("files/\(file.id)/subtitles/\(track.index)", method: "DELETE",
                query: [URLQueryItem(name: "track_id", value: id)])
            status = "AI subtitle deleted."
            await changed(id, nil)
        } catch { status = error.localizedDescription }
    }
}
