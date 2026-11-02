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

enum TVSettingsCategory: String, CaseIterable, Identifiable {
    case playback = "Playback"
    case help = "Help"
    case appearance = "Appearance"
    case liveTv = "Live TV"
    case sharing = "Sharing"
    case account = "Account"
    case remotes = "Remotes & devices"
    case developer = "Developer"
    case about = "About"
    var id: String { rawValue }
}

struct SettingsView: View {
    var category: TVSettingsCategory? = nil
    @FocusState private var focusedCategory: TVSettingsCategory?
    @State private var lastCategory = TVSettingsCategory.playback
    @State private var selectedCategory = TVSettingsCategory.playback
    @Namespace private var categoryFocus
    @EnvironmentObject var model: AppModel
    @EnvironmentObject private var remoteClient: RemoteClientModel
    @State private var diagnosticSignOutConfirmation = false
    // Graduated from Developer with the same keys and defaults, so a choice a
    // viewer already made there carries over unchanged.
    @AppStorage("plurx.boundedResume") private var boundedResumeEnabled = true
    @AppStorage("plurx.preparedHandoff") private var preparedHandoffEnabled = true
    #if os(tvOS)
    @AppStorage("plurx.liveTvLayout") private var liveTvLayoutRaw = TvLiveLayout.guidePreview.rawValue
    @AppStorage("plurx.tvHomeLayout") private var tvHomeLayoutRaw = TvHomeLayout.classic.rawValue
    #endif
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

    private var libraryGroupingLabel: String {
        #if os(tvOS)
        return "Library grouping"
        #else
        return "Home layout"
        #endif
    }

    @ViewBuilder var body: some View {
        #if os(tvOS)
        if category == nil {
            HStack(alignment: .top, spacing: 28) {
                List(TVSettingsCategory.allCases) { entry in
                    Button { selectedCategory = entry } label: {
                        HStack {
                            Text(entry.rawValue)
                            Spacer()
                            if selectedCategory == entry { Image(systemName: "checkmark") }
                        }
                    }
                    .focused($focusedCategory, equals: entry)
                    .prefersDefaultFocus(entry == lastCategory, in: categoryFocus)
                    .accessibilityIdentifier("settings-category-" + entry.id)
                    .buttonStyle(TVReadableButtonStyle(prominent: false))
                }
                .frame(width: 360)
                .focusScope(categoryFocus)
                .focusSection()
                .onChange(of: focusedCategory) { _, value in
                    if let value { lastCategory = value; selectedCategory = value }
                }
                Divider()
                VStack(alignment: .leading, spacing: 12) {
                    Text(selectedCategory.rawValue).font(.title2.bold()).padding(.horizontal, 24)
                    SettingsView(category: selectedCategory)
                        .id(selectedCategory)
                }
                .focusSection()
                .tvSettingsContentExit { focusedCategory = selectedCategory }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            .navigationTitle("Settings")
        } else { settingsForm }
        #else
        settingsForm
        #endif
    }

    private var settingsForm: some View {
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
                .accessibilityIdentifier("settings-quality")
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
                Toggle("Bounded pause/resume", isOn: $boundedResumeEnabled)
                Toggle("Prepared quality handoff", isOn: $preparedHandoffEnabled)
                NavigationLink("Pause/resume and quality handoff readiness") { PlaybackHandoffDetailsView() }
                NavigationLink("Captions and model setup") { ProgressiveCaptionSetupView() }
            } header: {
                Text("Playback")
            } footer: {
                Text("Track choices apply when a title has more than one. Instant subtitle switching prepares subtitles from the first frame; After a short pause builds them the first time you turn them on. Pause/resume and quality handoff are on by default; their readiness rows are advisory and never change either switch.")
            }
            .settingsCategory(.playback, selection: category)

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
                Picker("Layout", selection: Binding(
                    get: { model.layout }, set: { model.setLayout($0) }
                )) {
                    ForEach(ViewerLayout.allCases) { layout in Text(layout.label).tag(layout) }
                }
                .accessibilityIdentifier("settings-viewer-layout")
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
                #if os(tvOS)
                if model.layout == .standard {
                Picker("Home layout", selection: Binding(
                    get: { TvHomeLayout(rawValue: tvHomeLayoutRaw) ?? .classic },
                    set: { tvHomeLayoutRaw = $0.rawValue }
                )) {
                    ForEach(TvHomeLayout.allCases) { layout in
                        Text(layout.label).tag(layout)
                    }
                }
                .accessibilityIdentifier("settings-tv-home-layout")
                } else {
                    Text("Meridian provides its own featured Home. Your Standard Home choice is remembered.")
                        .foregroundStyle(Palette.muted)
                }
                #endif
                Picker(libraryGroupingLabel, selection: Binding(
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
                #if os(tvOS)
                Text("Classic keeps the current media shelves. Theater adds a featured title with a backdrop and Play or Resume controls. Library grouping and theme are independent of Home layout.")
                #else
                Text("Theme and room brightness are independent. Home layout groups shelves by media category or by individual library.")
                #endif
            }
            .settingsCategory(.appearance, selection: category)

            // Server Live TV settings, as on the web's Settings → Live TV. The
            // tuner, guide, recording and Library channel cards used to sit in
            // Developer; they are finished settings, so they live here, reached
            // the way Developer is: one focusable row that pushes the screen.
            Section {
                #if os(tvOS)
                Picker("Live TV layout", selection: Binding(
                    get: { TvLiveLayout(rawValue: liveTvLayoutRaw) ?? .guidePreview },
                    set: { liveTvLayoutRaw = $0.rawValue }
                )) {
                    ForEach(TvLiveLayout.allCases) { layout in
                        Text(layout.label).tag(layout)
                    }
                }
                #endif
                NavigationLink("On/off, tuner, guide, recording and Library channels") { LiveTvSettingsView() }
            } header: {
                Text("Live TV")
            } footer: {
                #if os(tvOS)
                Text("Layout changes presentation only; the current channel and tuner session continue playing. Server settings require administrator access.")
                #else
                Text("Server settings require administrator access.")
                #endif
            }
            .settingsCategory(.liveTv, selection: category)

            Section("Sharing") { NavigationLink("Shared libraries and Source status") { SharedSharingSettingsView() } }
                .settingsCategory(.sharing, selection: category)

            Section("Account") {
                LabeledContent("Signed in as", value: model.username ?? "—")
                LabeledContent("Server", value: serverSettingsLabel(name: model.serverName, hostname: model.serverHostname) ?? model.origin)
                Button("Change server") { model.changeServer() }
                Button("Sign out", role: .destructive) {
                    if DiagnosticReportModel.shared.hasPendingUnsubmitted { diagnosticSignOutConfirmation = true; return }
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
            .settingsCategory(.account, selection: category)

            Section("Remotes & devices") {
                NavigationLink("Manage remote pairings") { RemoteDeviceSettingsView() }
                Text(remoteClient.status).font(.caption)
            }
            .settingsCategory(.remotes, selection: category)
            Section("Developer") {
                NavigationLink("Shared libraries, Cinema remotes and other features awaiting evidence") { LiveTvDeveloperView() }
            }
            .settingsCategory(.developer, selection: category)

            Section("Help") {
                DiagnosticReportButton()
                Text("Save a private diagnostic ZIP on your phone, then attach it to your bug report.").font(.caption)
            }
            .settingsCategory(.help, selection: category)

            Section("About") {
                LabeledContent("App version", value: AppBuildInfo.current)
            }
            .settingsCategory(.about, selection: category)
        }
        #if os(tvOS)
        // The default Form picker pushes a choice list through the stack that
        // wraps the whole tab bar. On Apple TV that destination can be empty;
        // keep every settings choice in a focusable menu on this screen.
        .pickerStyle(.menu)
        .buttonStyle(TVReadableButtonStyle(prominent: false))
        #endif
        #if os(iOS)
        .navigationTitle("Settings")
        #endif
        .tint(Palette.accent)
        .background(Palette.bg)
        .alert("Clear pending local reports and sign out?", isPresented: $diagnosticSignOutConfirmation) {
            Button("Clear local reports and sign out", role: .destructive) { model.logout() }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Unsubmitted local captures and support credentials are cleared on sign-out. Export or finish a report first if you want to keep it. Already submitted reports remain at support.")
        }
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

/// Settings → Playback → "Pause/resume and quality handoff readiness". Both
/// switches graduated from Developer with their saved values (the same
/// `@AppStorage` keys and defaults) and their advisory rows. Each section is a
/// complete enable section: the switch, what it needs, and whether each part is
/// met — and no row here disables or overrides either switch.
struct PlaybackHandoffDetailsView: View {
    @AppStorage("plurx.boundedResume") private var boundedResumeEnabled = true
    @AppStorage("plurx.preparedHandoff") private var preparedHandoffEnabled = true
    @ObservedObject private var handoff = Caps.PreparedHandoffTelemetry.shared

    var body: some View {
        Form {
            Section("Bounded pause/resume") {
                Toggle("Enable bounded pause/resume", isOn: $boundedResumeEnabled)
                Text("On by default. Resume consumes a healthy retained buffer immediately and gives an established on-demand item one recipe-preserving repair inside one 15-second budget.")
                Label("Client ownership and deadline implementation: Met", systemImage: "checkmark.circle")
                Label("Server or protocol change required: Met · None", systemImage: "checkmark.circle")
                Label("Matched physical Apple TV latency: Not met", systemImage: "exclamationmark.triangle")
                Text("Qualification still needs matched fresh-open and pause/resume trials on the same Apple TV, delivery presentation, recipe, cache state, and network. This unmet row never disables or overrides the switch.")
                    .font(.caption)
                Text("Live TV, cold startup, AirPlay, and Picture in Picture keep their existing owners and budgets. A missing local video sample is not treated as a failed picture on an external display.")
                    .font(.caption)
            }
            Section("Prepared quality handoff") {
                Toggle("Enable two-player prepared handoff", isOn: $preparedHandoffEnabled)
                Text("On by default. This controls whether Apple advertises dual-player preparation. The checks below explain risk; they never disable or override the switch, and you can turn it off here.")
                Label("Client implementation and first-frame proof: Met", systemImage: "checkmark.circle")
                Label("Measured Apple cohort: Met", systemImage: "checkmark.circle")
                Text("iPhone 17 Pro Max and Apple TV 4K (3rd generation) each completed 20 of 20 same-codec and codec/HDR handoffs.")
                    .font(.caption)
                Label("Server successor media priming: Met", systemImage: "checkmark.circle")
                Text("The server reserves the successor before warming local or remote media and cleans up a refused, cancelled, or expired attempt.")
                    .font(.caption)
                Label("Fleet and link evidence: Not met · Checked during playback", systemImage: "exclamationmark.triangle")
                Text("Throughput, decoder capacity, and physical-device observations improve rollout confidence. Missing or low evidence never disables this switch.")
                    .font(.caption)
                Label("Last directed quality change: \(handoff.lastOutcome ?? "none yet")", systemImage: "info.circle")
                Text("What the most recent viewer-initiated quality change did. Informational only — it never blocks a change and never overrides the switch above.")
                    .font(.caption)
                Label("Last server preparation value: \(handoff.lastPreparation ?? "not sent")", systemImage: "info.circle")
                Text("The newest delivery.preparation this device saw. Older servers and relays do not send it, and its absence is never read as a refusal.")
                    .font(.caption)
                // M3's measurements of that same change, and the one audio
                // measurement this platform does not take. Advisory in the
                // strict sense: no switch is attached to any of it, nothing
                // reads it back, and an unmet condition never blocks a change.
                if let measured = handoff.lastSwitch {
                    Label("Frames at the switch: \(measured.frames)", systemImage: "info.circle")
                    Label("Audio at the switch: \(measured.audio)", systemImage: "info.circle")
                    Label("Tap to new quality: \(measured.visibleIn)", systemImage: "info.circle")
                }
                Text("Measuring the switch \u{2014} what it needs, and what it cannot have:")
                    .font(.caption)
                ForEach(PreparedSwitchMeasurement.audioRequirements(accessLogAvailable: true)) { requirement in
                    Label(
                        "\(requirement.title): \(requirement.met ? "Met" : "Not met")",
                        systemImage: requirement.met ? "checkmark.circle" : "exclamationmark.triangle"
                    )
                    Text(requirement.detail).font(.caption)
                }
            }
        }
        .navigationTitle("Pause and quality handoff")
    }
}

private extension View {
    @ViewBuilder
    func settingsCategory(_ value: TVSettingsCategory, selection: TVSettingsCategory?) -> some View {
        #if os(tvOS)
        if value == selection { self }
        #else
        self
        #endif
    }
}
