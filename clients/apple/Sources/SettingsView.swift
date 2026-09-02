import SwiftUI

/// A selectable language (id == the server's ISO 639-2/B code).
private struct Lang: Identifiable {
    let id: String
    let name: String
}

private let languages: [Lang] = [
    Lang(id: "eng", name: "English"), Lang(id: "jpn", name: "Japanese"),
    Lang(id: "spa", name: "Spanish"), Lang(id: "fre", name: "French"),
    Lang(id: "ger", name: "German"), Lang(id: "ita", name: "Italian"),
    Lang(id: "por", name: "Portuguese"), Lang(id: "kor", name: "Korean"),
    Lang(id: "chi", name: "Chinese"), Lang(id: "rus", name: "Russian"),
    Lang(id: "hin", name: "Hindi"), Lang(id: "ara", name: "Arabic"),
]
private let subtitleLanguages: [Lang] = [Lang(id: "off", name: "Off")] + languages

enum AppBuildInfo {
    static func label(version: String?, build: String?) -> String {
        let version = version?.trimmingCharacters(in: .whitespacesAndNewlines)
        let build = build?.trimmingCharacters(in: .whitespacesAndNewlines)
        let readableVersion = version.flatMap { $0.isEmpty ? nil : $0 } ?? "Unknown"
        guard let build, !build.isEmpty else { return readableVersion }
        return "\(readableVersion) (\(build))"
    }

    static var current: String {
        label(
            version: Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String,
            build: Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String
        )
    }
}

struct SettingsView: View {
    @EnvironmentObject var model: AppModel
    #if os(iOS)
    @State private var confirmingSignOut = false
    @State private var profileHasDownloads = false
    #endif

    private var audioBinding: Binding<String> {
        Binding(get: { model.audioLang },
                set: { model.setLanguages(audio: $0, sub: model.subLang) })
    }
    private var subBinding: Binding<String> {
        Binding(get: { model.subLang },
                set: { model.setLanguages(audio: model.audioLang, sub: $0) })
    }

    var body: some View {
        Form {
            Section {
                Picker("Quality", selection: Binding(
                    get: { model.playbackQuality },
                    set: { model.setPlaybackQuality($0) }
                )) {
                    ForEach(PlaybackQuality.allCases) { quality in
                        Text(quality.label).tag(quality)
                    }
                }
                Picker("Audio language", selection: audioBinding) {
                    ForEach(languages) { Text($0.name).tag($0.id) }
                }
                Picker("Subtitle language", selection: subBinding) {
                    ForEach(subtitleLanguages) { Text($0.name).tag($0.id) }
                }
                Picker("Subtitle switching", selection: Binding(
                    get: { model.subtitleReadiness },
                    set: { model.setSubtitleReadiness($0) }
                )) {
                    ForEach(SubtitleReadiness.allCases) { readiness in
                        Text(readiness.label).tag(readiness)
                    }
                }
                Toggle("Autoplay next episode", isOn: Binding(
                    get: { model.autoplay },
                    set: { model.setAutoplay($0) }
                ))
                Toggle("Skip intros and credits", isOn: Binding(
                    get: { model.autoSkip },
                    set: { model.setAutoSkip($0) }
                ))
            } header: {
                Text("Playback")
            } footer: {
                Text("Track choices apply when a title has more than one. Instant subtitle switching prepares subtitles from the first frame; After a short pause builds them the first time you turn them on.")
            }

            #if os(iOS)
            Section {
                Picker("Download quality", selection: Binding(
                    get: { model.offlineQuality },
                    set: { model.setOfflineQuality($0) }
                )) {
                    ForEach(OfflineQuality.allCases) { quality in
                        Text(quality.label).tag(quality)
                    }
                }
                Picker("Download over", selection: Binding(
                    get: { model.offlineNetwork },
                    set: { model.setOfflineNetwork($0) }
                )) {
                    ForEach(OfflineNetworkPolicy.allCases) { policy in
                        Text(policy.label).tag(policy)
                    }
                }
            } header: {
                Text("Downloads")
            } footer: {
                Text("Standard uses up to 720p and less storage. High uses up to 1080p. A network change applies to the next download.")
            }
            #endif

            Section {
                Picker("Theme", selection: Binding(
                    get: { model.theme },
                    set: { model.setTheme($0) }
                )) {
                    ForEach(ViewerTheme.allCases) { theme in
                        Text(theme.label).tag(theme)
                    }
                }
                Picker("Appearance", selection: Binding(
                    get: { model.appearance },
                    set: { model.setAppearance($0) }
                )) {
                    ForEach(ViewerAppearance.allCases) { appearance in
                        Text(appearance.label).tag(appearance)
                    }
                }
                Picker("Poster size", selection: Binding(
                    get: { model.posterSize },
                    set: { model.setPosterSize($0) }
                )) {
                    ForEach(PosterSize.allCases) { size in
                        Text(size.label).tag(size)
                    }
                }
                Picker("Home layout", selection: Binding(
                    get: { model.libraryGrouping },
                    set: { model.setLibraryGrouping($0) }
                )) {
                    ForEach(LibraryGrouping.allCases) { grouping in
                        Text(grouping.label).tag(grouping)
                    }
                }
            } header: {
                Text("Appearance")
            } footer: {
                Text("Theme and room brightness are independent. Home layout groups shelves by media category or by individual library.")
            }

            Section("Account") {
                LabeledContent("Signed in as", value: model.username ?? "—")
                LabeledContent("Server", value: model.serverName ?? model.origin)
                Button("Change server") { model.changeServer() }
                Button("Sign out", role: .destructive) {
                    #if os(iOS)
                    Task {
                        async let video = OfflineDownloadManager.shared.hasCurrentProfileDownloads()
                        async let books = OfflineBookManager.shared.hasCurrentProfileDownloads()
                        let hasVideo = await video
                        let hasBooks = await books
                        profileHasDownloads = hasVideo || hasBooks
                        if profileHasDownloads {
                            confirmingSignOut = true
                        } else {
                            model.logout()
                        }
                    }
                    #else
                    model.logout()
                    #endif
                }
            }

            Section("About") {
                LabeledContent("App version", value: AppBuildInfo.current)
            }
        }
        .navigationTitle("Settings")
        .tint(Palette.accent)
        .background(Palette.bg)
        #if os(iOS)
        .scrollContentBackground(.hidden)
        .navigationBarTitleDisplayMode(.inline)
        .confirmationDialog(
            "Keep this profile's downloads?",
            isPresented: $confirmingSignOut,
            titleVisibility: .visible
        ) {
            Button("Keep downloads and sign out") { model.logout() }
            Button("Remove downloads and sign out", role: .destructive) {
                Task {
                    await OfflineDownloadManager.shared.removeCurrentProfile()
                    await OfflineBookManager.shared.removeCurrentProfile()
                    model.logout()
                }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Kept downloads stay on this device and reappear when this Cinema profile signs in again.")
        }
        #endif
    }
}
