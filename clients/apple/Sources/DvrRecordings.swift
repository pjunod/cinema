import SwiftUI

// The DVR as a page: what is planned, what was kept, and the rules that put it
// there. Everything here reads rows and writes intent — the owner node is the
// only thing that touches a tuner or a file, so a Stop here records a request
// and the list says "stopping" until the owner's tick has closed the capture.

// MARK: - State

/// One authenticated server/profile DVR shared by every app surface. The
/// controller owns the only observation timer; views subscribe to published
/// snapshots and never create badge-sized polling loops of their own.
@MainActor
final class DvrController: ObservableObject {
    static let shared = DvrController()

    @Published private(set) var status: DvrStatus?
    @Published private(set) var overview: DvrOverview?
    @Published private(set) var overviewError: String?
    @Published private(set) var marks = DvrMarks()
    @Published private(set) var schedule: [DvrRecording] = []
    @Published private(set) var conflicts = 0
    @Published private(set) var library: [DvrRecording] = []
    @Published private(set) var rules: [DvrRule] = []
    /// Reminders the server has fired and nobody has acknowledged yet.
    @Published private(set) var due: [DvrReminder] = []
    /// A finished recording the viewer asked to remove. The server refuses the
    /// first request because "take this off my schedule" and "delete the file"
    /// must not be the same press by accident; this is the second press.
    @Published private(set) var confirmFileDelete: DvrRecording?
    /// What the last action said, kept until the next one — exactly as the
    /// Live TV page keeps its own line, because a toast that vanishes cannot
    /// repeat itself when the same refusal happens twice.
    @Published private(set) var message: String?
    @Published private(set) var busy = false
    @Published private(set) var reminders: [DvrReminder] = []
    /// Fired reminders this device has stopped showing without acknowledging
    /// them. The server still has them fired, so the television still gets its
    /// chance to say something about the same programme.
    @Published private(set) var silenced: Set<String> = []
    @Published private(set) var attention: [DvrAttentionProjection] = []
    @Published private(set) var attentionTotal = 0
    @Published private(set) var attentionNext: String?
    @Published private(set) var selectedRecording: DvrRecording?
    @Published private(set) var selectedEvents: [DvrEvent] = []
    @Published private(set) var selectedEventsNext: String?
    @Published private(set) var selectedHistoryComplete = false
    @Published private(set) var selectedHistoryTruncatedBefore: Int64?

    private var api: DvrAPI?
    private var profileOrigin: String?
    private var profileToken: String?
    private var scopeGeneration: UInt64 = 0
    private var overviewLoading = false
    private var overviewQueued = false
    private var overviewFailures = 0
    private var lastScheduleRead = Date.distantPast
    private var lastActiveIdentity = Set<String>()

    var enabled: Bool { status?.enabled ?? false }

    var hasObservedActivity: Bool {
        guard let overview else { return false }
        return overview.counts.active > 0 || overview.nextCaptureStart.map { $0 <= Int(Date().timeIntervalSince1970) + 30 } == true
    }

    var indicatorLabel: String? {
        guard let overview else { return nil }
        guard overview.fresh else {
            return overview.counts.active > 0 ? "Status unavailable" : nil
        }
        var facts: [String] = []
        func append(_ count: Int?, _ label: String) {
            if let count, count > 0 { facts.append("\(count) \(label)") }
        }
        append(overview.counts.recording, "recording")
        append(overview.counts.starting, "starting")
        append(overview.counts.reconnecting, "reconnecting")
        append(overview.counts.finishing, "finishing")
        append(overview.counts.unconfirmed, "unconfirmed")
        return facts.isEmpty ? nil : facts.joined(separator: " · ")
    }

    /// The one reminder the overlay is showing, if any.
    var current: DvrReminder? {
        due.first { !silenced.contains($0.id) }
    }

    /// An airing is `(channel, start)` for its whole life, here as everywhere
    /// else in this feature.
    func recording(channelId: String, airingStart: Int) -> DvrRecording? {
        schedule.first { $0.channelId == channelId && $0.airingStart == airingStart }
    }

    func activeRecording(channelId: String, airingStart: Int) -> DvrActiveRecording? {
        overview?.active.first { $0.channelId == channelId && $0.airingStart == airingStart }
    }

    func reminder(channelId: String, airingStart: Int) -> DvrReminder? {
        reminders.first {
            $0.channelId == channelId && $0.airingStart == airingStart && $0.state == .armed
        }
    }

    func silence(_ reminder: DvrReminder) {
        silenced.insert(reminder.id)
    }

    /// The line is the answer to something the viewer just pressed, so it
    /// stops being true the moment they move on to something else.
    func clearMessage() {
        message = nil
    }

    func load(origin: String, token: String?) async {
        if profileOrigin != origin || profileToken != token || api == nil {
            api = DvrAPI(origin: origin, token: token)
            profileOrigin = origin
            profileToken = token
            scopeGeneration &+= 1
            status = nil
            overview = nil
            overviewError = nil
            schedule = []
            library = []
            rules = []
            attention = []
            selectedRecording = nil
            selectedEvents = []
            marks = DvrMarks()
            overviewFailures = 0
            lastScheduleRead = .distantPast
            lastActiveIdentity = []
        }
        guard let api else { return }
        let generation = scopeGeneration
        // A DVR that is switched off is a rendered state, not a failure: the
        // marks stay empty, the actions say why, and the guide is unaffected.
        let fetchedStatus = try? await api.status()
        guard generation == scopeGeneration else { return }
        status = fetchedStatus
        await refresh()
        await refreshOverview()
    }

    /// The app shell runs this while foregrounded. Cancellation on background
    /// ends the loop immediately; reminders keep their existing independent
    /// delivery path. Failed reads retain the last snapshot and back off.
    func observe(origin: String, token: String?, highFrequency: Bool = false) async {
        await load(origin: origin, token: token)
        while !Task.isCancelled {
            await refreshOverview()
            let base = highFrequency || hasObservedActivity ? 5 : 30
            let backoff = overviewFailures == 0 ? base : min(30, 5 << min(overviewFailures - 1, 3))
            do {
                try await Task.sleep(for: .seconds(max(base, backoff)))
            } catch {
                return
            }
        }
    }

    /// The two reads every mark is drawn from.
    func refresh() async {
        guard let api else { return }
        let generation = scopeGeneration
        if let plan = try? await api.schedule(days: 14, cancelled: true),
           generation == scopeGeneration {
            schedule = plan.rows
            conflicts = plan.conflicts
            lastScheduleRead = Date()
        }
        if let rows = try? await api.reminders(), generation == scopeGeneration {
            reminders = rows
            due = rows.filter { $0.state == .fired }
        }
        marks = DvrMarks(schedule: schedule, reminders: reminders)
        #if os(iOS)
        await LocalReminders.shared.mirror(reminders)
        #endif
    }

    func refreshOverview() async {
        guard let api else { return }
        guard !overviewLoading else {
            overviewQueued = true
            return
        }
        overviewLoading = true
        let generation = scopeGeneration
        do {
            let fetched = try await api.overview()
            guard generation == scopeGeneration else {
                overviewLoading = false
                return
            }
            overview = fetched
            overviewError = nil
            overviewFailures = 0
            let identity = Set(fetched.active.map(\.recordingId))
            if identity != lastActiveIdentity || Date().timeIntervalSince(lastScheduleRead) >= 30 {
                lastActiveIdentity = identity
                await refresh()
            }
        } catch {
            if generation == scopeGeneration {
                overviewError = error.localizedDescription
                overviewFailures += 1
            }
        }
        overviewLoading = false
        if overviewQueued {
            overviewQueued = false
            await refreshOverview()
        }
    }

    /// Only the fired list, and only on the page's own thirty-second tick.
    ///
    /// The marks are never polled, because a plan changes when somebody
    /// changes it and every mutation re-reads for itself. A reminder is the
    /// one thing here that becomes true on the server's clock rather than on
    /// anybody's press, so nothing else on this page could ever notice it.
    func refreshDue() async {
        guard let api else { return }
        guard let rows = try? await api.reminders(due: true) else { return }
        due = rows
    }

    func loadLibrary() async {
        guard let api else { return }
        library = (try? await api.recordings(states: [.done, .partial])) ?? []
    }

    func loadAttention(more: Bool = false) async {
        guard let api else { return }
        let generation = scopeGeneration
        do {
            let page = try await api.attention(after: more ? attentionNext : nil)
            guard generation == scopeGeneration else { return }
            attention = more ? attention + page.rows : page.rows
            attentionNext = page.next
            attentionTotal = page.total
        } catch {
            message = error.localizedDescription
        }
    }

    func selectRecording(_ id: String) async {
        guard let api else { return }
        let generation = scopeGeneration
        do {
            async let row = api.recording(id)
            async let page = api.events(id)
            let (fetchedRow, fetchedPage) = try await (row, page)
            guard generation == scopeGeneration else { return }
            selectedRecording = fetchedRow
            selectedEvents = fetchedPage.rows
            selectedEventsNext = fetchedPage.next
            selectedHistoryComplete = fetchedPage.historyComplete
            selectedHistoryTruncatedBefore = fetchedPage.truncatedBeforeSequence
        } catch {
            message = error.localizedDescription
        }
    }

    func loadOlderSelectedEvents() async {
        guard let api, let row = selectedRecording, let before = selectedEventsNext else { return }
        do {
            let page = try await api.events(row.id, before: before)
            selectedEvents += page.rows
            selectedEventsNext = page.next
            selectedHistoryComplete = page.historyComplete
            selectedHistoryTruncatedBefore = page.truncatedBeforeSequence
        } catch {
            message = error.localizedDescription
        }
    }

    func refreshSelectedEvents() async {
        guard let api, let row = selectedRecording,
              let latest = selectedEvents.map(\.sequence).max()
        else { return }
        do {
            async let refreshedRow = api.recording(row.id)
            async let page = api.events(row.id, after: String(latest))
            let (fetchedRow, fetchedPage) = try await (refreshedRow, page)
            selectedRecording = fetchedRow
            if !fetchedPage.rows.isEmpty {
                let known = Set(selectedEvents.map(\.eventId))
                selectedEvents = fetchedPage.rows.reversed().filter { !known.contains($0.eventId) }
                    + selectedEvents
            }
        } catch {
            overviewError = error.localizedDescription
        }
    }

    func acknowledgeSelectedAttention() async {
        guard let api, let row = selectedRecording,
              let latest = selectedEvents.map(\.sequence).max()
        else { return }
        do {
            _ = try await api.acknowledgeAttention(row.id, through: latest)
            await loadAttention()
            message = "Marked \(row.title) reviewed."
        } catch {
            message = error.localizedDescription
        }
    }

    func loadRules() async {
        guard let api else { return }
        rules = (try? await api.rules()) ?? []
    }

    // MARK: Airing actions

    func record(channelId: String, airingStart: Int) async {
        await act { api in
            let row = try await api.record(channelId: channelId, airingStart: airingStart)
            return "Recording \(row.title) at \(liveTvTime(row.airingStart))."
        }
    }

    func recordSeries(channelId: String, airingStart: Int) async {
        await act { api in
            let rule = try await api.createRule(channelId: channelId, airingStart: airingStart)
            // Say which kind of rule it became. A title rule locked to one
            // channel and a series-id rule behave differently the first time
            // the broadcaster renames the programme.
            return "Recording the series \(rule.name) · \(rule.matchMode.label)."
        }
    }

    func recordManual(channelId: String, start: Date, end: Date, title: String) async {
        await act { api in
            let row = try await api.record(
                channelId: channelId.trimmingCharacters(in: .whitespacesAndNewlines),
                captureStart: Int(start.timeIntervalSince1970),
                captureEnd: Int(end.timeIntervalSince1970),
                title: title
            )
            return "Recording \(row.channelName) from \(liveTvTime(row.captureStart)) to \(liveTvTime(row.captureEnd))."
        }
    }

    func remind(channelId: String, airingStart: Int) async {
        #if os(iOS)
        // Asked here and nowhere else. A permission sheet at launch, before
        // the viewer has asked for a single reminder, is a sheet they decline.
        await LocalReminders.shared.requestAuthorization()
        #endif
        // Deliberately not gated on the DVR switch: a reminder is a row with a
        // time in it, and it needs neither a tuner nor a disk.
        await act(requiresDvr: false) { api in
            let reminder = try await api.remind(channelId: channelId, airingStart: airingStart)
            return "Reminder set for \(reminder.title) at \(liveTvTime(reminder.airingStart))."
        }
    }

    // MARK: Row actions

    func delete(_ row: DvrRecording, deleteFile: Bool = false) async {
        confirmFileDelete = nil
        guard let api else { return }
        busy = true
        do {
            switch try await api.delete(row.id, deleteFile: deleteFile) {
            case .removed:
                message = row.state.hasMedia
                    ? "Deleted \(row.title) and its file."
                    : "Skipped \(row.title)."
            case .stopping:
                message = "Stopping \(row.title). The recording closes on the owner's next tick."
            }
            await refresh()
            await refreshOverview()
            if row.state.hasMedia { await loadLibrary() }
        } catch let failure as DvrFailure where failure.code == "delete_file_required" {
            confirmFileDelete = row
            message = failure.localizedDescription
        } catch {
            message = error.localizedDescription
        }
        busy = false
    }

    func dismissFileDelete() {
        confirmFileDelete = nil
    }

    func restore(_ row: DvrRecording) async {
        await act { api in
            let restored = try await api.restore(row.id)
            return "Restored \(restored.title) at \(liveTvTime(restored.airingStart))."
        }
    }

    func ack(_ reminder: DvrReminder) async {
        guard let api else { return }
        try? await api.ackReminder(reminder.id)
        await refresh()
    }

    func forget(_ reminder: DvrReminder) async {
        await act(requiresDvr: false) { api in
            try await api.deleteReminder(reminder.id)
            return "Reminder for \(reminder.title) removed."
        }
    }

    // MARK: Rule actions

    func setRule(_ rule: DvrRule, enabled: Bool) async {
        await act { api in
            _ = try await api.updateRule(rule.id, .enabled(enabled))
            return enabled ? "\(rule.name) is on." : "\(rule.name) is off."
        }
        await loadRules()
    }

    /// First runs only, or every showing. The one rule setting a viewer
    /// changes their mind about often enough to be worth a press.
    func setRule(_ rule: DvrRule, newOnly: Bool) async {
        await act { api in
            _ = try await api.updateRule(rule.id, .newOnly(newOnly))
            return newOnly
                ? "\(rule.name) records new episodes only."
                : "\(rule.name) records every showing."
        }
        await loadRules()
    }

    func deleteRule(_ rule: DvrRule) async {
        await act { api in
            try await api.deleteRule(rule.id)
            // Not "cancelled": the airings it put on the schedule are
            // withdrawn on the owner's next tick, and cancelled is a word that
            // belongs to a person's decision about one episode.
            return "Deleted the rule \(rule.name). Its planned episodes are withdrawn."
        }
        await loadRules()
    }

    /// Priority is a server-wide decision — it settles which of two rules gets
    /// the tuner — so the server refuses this to anyone but an administrator,
    /// and the refusal is what the viewer is shown.
    func moveRule(_ rule: DvrRule, by delta: Int) async {
        var ids = rules.map(\.id)
        guard let at = ids.firstIndex(of: rule.id) else { return }
        let target = at + delta
        guard ids.indices.contains(target) else { return }
        ids.swapAt(at, target)
        await act { api in
            self.rules = try await api.reorderRules(ids)
            return "Rule order saved."
        }
    }

    /// Every write shares one shape: run it, say what happened in the viewer's
    /// words, and re-read the two lists the marks come from.
    private func act(requiresDvr: Bool = true,
                     _ operation: (DvrAPI) async throws -> String) async {
        guard let api else { return }
        guard !requiresDvr || enabled || status == nil else {
            message = DvrFailure(code: "dvr_disabled").localizedDescription
            return
        }
        busy = true
        do {
            message = try await operation(api)
            await refresh()
            await refreshOverview()
        } catch {
            message = error.localizedDescription
        }
        busy = false
    }
}

// MARK: - Cell marks

enum DvrMarkMetrics {
    #if os(tvOS)
    static let size: CGFloat = 12
    #else
    static let size: CGFloat = 8
    #endif
}

/// The corner glyph for one airing. Drawn as an overlay so it costs the grid
/// no layout at all: the rows, the slot width and the type sizes are exactly
/// what they were before recording existed.
struct DvrCellMarkView: View {
    let mark: DvrCellMark

    private var size: CGFloat { DvrMarkMetrics.size }

    var body: some View {
        switch mark {
        case .scheduled(let series):
            HStack(spacing: 2) {
                dot(Palette.accent)
                if series { dot(Palette.accent) }
            }
        case .recording:
            Text("REC")
                .font(.system(size: size, weight: .bold))
                .foregroundStyle(.white)
                .padding(.horizontal, 3)
                .background(.red, in: RoundedRectangle(cornerRadius: 3))
        case .conflict:
            dot(.orange)
        case .lapsed:
            Circle()
                .stroke(Palette.muted, lineWidth: 1)
                .frame(width: size, height: size)
        case .reminder:
            Image(systemName: "bell.fill")
                .font(.system(size: size))
                .foregroundStyle(Palette.muted)
        }
    }

    private func dot(_ color: Color) -> some View {
        Circle().fill(color).frame(width: size, height: size)
    }
}

extension View {
    /// One cell's mark: a glyph in the corner and, for a capture that is
    /// running, a line along the bottom that fills as the window closes. Both
    /// are overlays, so a cell with a mark is the same size as one without.
    @ViewBuilder
    func dvrCellMark(_ mark: DvrCellMark?, now: Int) -> some View {
        if let mark {
            self
                .overlay(alignment: .topTrailing) {
                    DvrCellMarkView(mark: mark).padding(3)
                }
                .overlay(alignment: .bottom) {
                    if case let .recording(start, end) = mark {
                        LiveTvProgressLine(
                            value: end > start
                                ? min(max(Double(now - start) / Double(end - start), 0), 1)
                                : 0,
                            height: 2
                        )
                    }
                }
        } else {
            self
        }
    }
}

// MARK: - Actions

/// One action, sized for whichever surface it lands on. Repeating the platform
/// branch on a dozen buttons is how a phone control ends up sixty-six points
/// tall on a television.
struct DvrActionButton: View {
    let label: String
    var prominent = false
    var systemImage: String?
    let action: () -> Void

    @ViewBuilder var body: some View {
        #if os(tvOS)
        button
            .buttonStyle(TVReadableButtonStyle(prominent: prominent, compact: true))
            .focusEffectDisabled()
        #else
        // Two branches rather than one expression: `.bordered` and
        // `.borderedProminent` are different types, and erasing them to pick
        // between them costs more than writing both.
        if prominent {
            button.font(.system(size: 13, weight: .semibold)).buttonStyle(.borderedProminent)
        } else {
            button.font(.system(size: 13, weight: .semibold)).buttonStyle(.bordered)
        }
        #endif
    }

    private var button: some View {
        Button(action: action) {
            if let systemImage {
                Label(label, systemImage: systemImage)
            } else {
                Text(label)
            }
        }
    }
}

// MARK: - The page

enum DvrRecordingsChip: String, CaseIterable, Identifiable {
    case saved
    case upcoming
    case attention
    case rules
    case manual
    case skipped
    case reminders

    var id: String { rawValue }
    var label: String {
        switch self {
        case .saved: return "Saved"
        case .upcoming: return "Upcoming"
        case .attention: return "Needs attention"
        case .rules: return "Series rules"
        case .manual: return "Manual recording"
        case .skipped: return "Skipped"
        case .reminders: return "Reminders"
        }
    }
}

/// Library · Scheduled · Rules, beside the guide rather than behind a separate
/// page: a viewer deciding what to record and a viewer checking what will be
/// recorded are the same person two seconds apart.
struct DvrRecordingsPanel: View {
    @ObservedObject var dvr: DvrController
    let now: Int
    @State private var chip = DvrRecordingsChip.upcoming
    @State private var manualChannel = ""
    @State private var manualTitle = ""
    @State private var manualStart = Date().addingTimeInterval(300)
    @State private var manualEnd = Date().addingTimeInterval(3_900)
    @State private var manualStartDelay = 5
    @State private var manualDuration = 60

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                ForEach(DvrRecordingsChip.allCases) { entry in
                    chipButton(entry)
                }
                Spacer(minLength: 8)
                if let summary = summary { Text(summary).font(LiveTvType.tertiary).foregroundStyle(Palette.muted) }
            }
            if let serverLine = serverLine { Text(serverLine).font(LiveTvType.tertiary).foregroundStyle(Palette.muted) }
            if let row = dvr.confirmFileDelete {
                confirmation(row)
            }
            content
        }
        .frame(maxWidth: .infinity, alignment: .topLeading)
        // Only the phone needs this. On a television Live TV already sits
        // inside the shell's navigation stack, which registers the same
        // destination; declaring it twice there would be two answers to one
        // route.
        #if os(iOS)
        .navigationDestination(for: Route.self) { route in
            switch route {
            case .item(let id): DetailView(itemId: id)
            case .collection(let collection): LibraryView(collection: collection)
            }
        }
        #endif
        .task(id: chip) {
            switch chip {
            case .saved: await dvr.loadLibrary()
            case .rules: await dvr.loadRules()
            case .attention: await dvr.loadAttention()
            case .upcoming, .skipped, .reminders, .manual: await dvr.refresh()
            }
        }
    }

    /// The standing facts about the server's recorder: how many tuners it is
    /// holding, how much room is left against the floor it will not record
    /// past, and when it next wakes up. A viewer who cannot see the floor
    /// finds out about it as a missing programme.
    private var serverLine: String? {
        guard let status = dvr.status else { return nil }
        guard status.enabled else {
            return "Recording is off. An administrator can enable it in Settings → Developer."
        }
        var facts = [
            "\(status.slots.recording) of \(status.slots.max) tuners recording",
            "\(status.slots.reserve) reserved",
        ]
        if let free = status.freeBytes {
            let room = ByteCountFormatter.string(fromByteCount: free, countStyle: .file)
            facts.append(free < status.floorBytes
                         ? "\(room) free — below the floor, so nothing new will start"
                         : "\(room) free")
        }
        if let next = status.nextStart { facts.append("next at \(liveTvTime(next))") }
        return facts.joined(separator: " · ")
    }

    private var summary: String? {
        switch chip {
        case .upcoming:
            guard dvr.conflicts > 0 else { return nil }
            return dvr.conflicts == 1 ? "1 clash" : "\(dvr.conflicts) clashes"
        case .saved:
            return dvr.library.isEmpty ? nil : "\(dvr.library.count) recordings"
        case .attention:
            return dvr.attentionTotal == 0 ? nil : "\(dvr.attentionTotal) to review"
        case .rules:
            return dvr.rules.isEmpty ? nil : "in priority order · lower wins"
        case .manual, .skipped, .reminders:
            return nil
        }
    }

    private func chipButton(_ entry: DvrRecordingsChip) -> some View {
        Button { chip = entry } label: {
            #if os(tvOS)
            // The television style already owns the selected plate; a second
            // background behind the label would be a plate inside a plate.
            Text(entry.label)
            #else
            Text(entry.label)
                .font(.system(size: 13, weight: .semibold))
                .padding(.horizontal, 12)
                .frame(height: 32)
                .background(chip == entry ? Palette.surfaceHi : Color.clear,
                            in: RoundedRectangle(cornerRadius: 8))
            #endif
        }
        #if os(tvOS)
        .buttonStyle(TVReadableButtonStyle(prominent: chip == entry, compact: true))
        .focusEffectDisabled()
        #else
        .buttonStyle(.plain)
        .foregroundStyle(chip == entry ? Palette.onBg : Palette.muted)
        #endif
    }

    private func confirmation(_ row: DvrRecording) -> some View {
        HStack(spacing: 10) {
            Text("Delete \(row.title) and its file?")
                .font(LiveTvType.secondary).lineLimit(2)
            DvrActionButton(label: "Delete the file", prominent: true) {
                Task { await dvr.delete(row, deleteFile: true) }
            }
            DvrActionButton(label: "Keep it") { dvr.dismissFileDelete() }
        }
        .padding(10)
        .background(Palette.surface, in: RoundedRectangle(cornerRadius: 8))
    }

    @ViewBuilder private var content: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 6) {
                switch chip {
                case .saved:
                    if dvr.library.isEmpty { empty("Nothing recorded yet.") }
                    ForEach(dvr.library) { libraryRow($0) }
                case .upcoming:
                    if dvr.schedule.isEmpty { empty("Nothing is scheduled.") }
                    ForEach(dvr.schedule.filter { $0.state != .cancelled }
                        .sorted { ($0.captureStart, $0.id) < ($1.captureStart, $1.id) }) { scheduleRow($0) }
                case .attention:
                    if dvr.attention.isEmpty { empty("Nothing needs review.") }
                    ForEach(dvr.attention) { scheduleRow($0.recording) }
                case .rules:
                    if dvr.rules.isEmpty {
                        empty("No series rules. Record series on a guide cell makes the first one.")
                    }
                    ForEach(dvr.rules) { ruleRow($0) }
                case .skipped:
                    let rows = dvr.schedule.filter { $0.state == .cancelled }
                    if rows.isEmpty { empty("No skipped airings.") }
                    ForEach(rows.sorted { ($0.captureStart, $0.id) < ($1.captureStart, $1.id) }) {
                        scheduleRow($0)
                    }
                case .reminders:
                    if dvr.reminders.isEmpty { empty("No reminders. Add one from a guide airing.") }
                    ForEach(dvr.reminders) { reminderRow($0) }
                case .manual:
                    manualRecording
                }
            }
            .padding(.trailing, 4)
        }
    }

    private func empty(_ text: String) -> some View {
        Text(text)
            .font(LiveTvType.secondary)
            .foregroundStyle(Palette.muted)
            .padding(.vertical, 8)
    }

    // MARK: Rows

    @ViewBuilder private func libraryRow(_ row: DvrRecording) -> some View {
        rowFrame {
            VStack(alignment: .leading, spacing: 2) {
                NavigationLink {
                    DvrRecordingDetailView(recordingId: row.id)
                } label: {
                    Text(row.title).font(LiveTvType.primary).lineLimit(1)
                }
                .buttonStyle(.plain)
                Text(libraryDetail(row))
                    .font(LiveTvType.tertiary).foregroundStyle(Palette.muted).lineLimit(1)
            }
            Spacer(minLength: 8)
            if let itemId = row.itemId {
                NavigationLink(value: Route.item(itemId)) { Text("Play") }
                    .buttonStyle(.plain)
            }
            DvrActionButton(label: "Delete") { Task { await dvr.delete(row) } }
        }
    }

    private func libraryDetail(_ row: DvrRecording) -> String {
        var facts = ["\(row.guideNumber) \(row.channelName)", liveTvTime(row.airingStart)]
        if let episode = row.episode { facts.append(episode) }
        if row.bytes > 0 {
            facts.append(ByteCountFormatter.string(fromByteCount: row.bytes, countStyle: .file))
        }
        if row.state == .partial {
            facts.append(row.gapS > 0 ? "gap of \(row.gapS / 60) min" : "late start")
        }
        if row.itemId == nil { facts.append("not in the library yet") }
        return facts.joined(separator: " · ")
    }

    @ViewBuilder private func scheduleRow(_ row: DvrRecording) -> some View {
        rowFrame {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    NavigationLink {
                        DvrRecordingDetailView(recordingId: row.id)
                    } label: {
                        Text(row.title).font(LiveTvType.primary).lineLimit(1)
                    }
                    .buttonStyle(.plain)
                    if row.ruleId != nil {
                        Text("SERIES").font(LiveTvType.eyebrow).foregroundStyle(Palette.muted)
                    }
                }
                Text(scheduleDetail(row))
                    .font(LiveTvType.tertiary).foregroundStyle(Palette.muted).lineLimit(2)
                if row.state == .recording {
                    LiveTvProgressLine(value: row.progress(now: now), height: 3)
                }
            }
            Spacer(minLength: 8)
            scheduleActions(row)
        }
    }

    private func scheduleDetail(_ row: DvrRecording) -> String {
        var facts = [
            "\(row.guideNumber) \(row.channelName)",
            "\(liveTvTime(row.airingStart))–\(liveTvTime(row.airingEnd))",
            row.stopping ? "Stopping" : row.state.label,
        ]
        // A conflict without its reason is an accusation with no evidence:
        // the row says which rule or which other airing took the tuner.
        if let reason = row.stateReason, row.state != .scheduled { facts.append(reason) }
        return facts.joined(separator: " · ")
    }

    @ViewBuilder private func scheduleActions(_ row: DvrRecording) -> some View {
        switch row.state {
        case .recording:
            DvrActionButton(label: row.stopping ? "Stopping…" : "Stop") {
                Task { await dvr.delete(row) }
            }
        case .cancelled:
            DvrActionButton(label: "Restore") { Task { await dvr.restore(row) } }
        default:
            DvrActionButton(label: "Skip") { Task { await dvr.delete(row) } }
        }
    }

    @ViewBuilder private func ruleRow(_ rule: DvrRule) -> some View {
        rowFrame {
            VStack(alignment: .leading, spacing: 2) {
                Text(rule.name).font(LiveTvType.primary).lineLimit(1)
                Text(ruleDetail(rule))
                    .font(LiveTvType.tertiary).foregroundStyle(Palette.muted).lineLimit(2)
            }
            Spacer(minLength: 8)
            DvrActionButton(label: "▲") { Task { await dvr.moveRule(rule, by: -1) } }
            DvrActionButton(label: "▼") { Task { await dvr.moveRule(rule, by: 1) } }
            DvrActionButton(label: rule.newOnly ? "New only" : "Every showing") {
                Task { await dvr.setRule(rule, newOnly: !rule.newOnly) }
            }
            DvrActionButton(label: rule.enabled ? "On" : "Off", prominent: rule.enabled) {
                Task { await dvr.setRule(rule, enabled: !rule.enabled) }
            }
            DvrActionButton(label: "Delete") { Task { await dvr.deleteRule(rule) } }
        }
    }

    private func ruleDetail(_ rule: DvrRule) -> String {
        [
            rule.matchMode.label,
            rule.channelId == nil ? "any channel" : "one channel",
            rule.newOnly ? "new episodes only" : "every showing",
            rule.keepMode.label(value: rule.keepValue),
        ].joined(separator: " · ")
    }

    private var manualRecording: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Record one channel for an exact time range, even beyond the guide horizon.")
                .font(LiveTvType.secondary).foregroundStyle(Palette.muted)
            TextField("Channel ID", text: $manualChannel)
            TextField("Title (optional)", text: $manualTitle)
            #if os(iOS)
            DatePicker("Starts", selection: $manualStart)
            DatePicker("Ends", selection: $manualEnd)
            #else
            Picker("Starts", selection: $manualStartDelay) {
                Text("Now").tag(0)
                Text("In 5 minutes").tag(5)
                Text("In 15 minutes").tag(15)
            }
            Picker("Duration", selection: $manualDuration) {
                ForEach([30, 60, 90, 120], id: \.self) { minutes in
                    Text("\(minutes) minutes").tag(minutes)
                }
            }
            #endif
            DvrActionButton(label: "Schedule recording", prominent: true) {
                Task {
                    #if os(iOS)
                    let start = manualStart
                    let end = manualEnd
                    #else
                    let start = Date().addingTimeInterval(Double(manualStartDelay * 60))
                    let end = start.addingTimeInterval(Double(manualDuration * 60))
                    #endif
                    await dvr.recordManual(channelId: manualChannel, start: start,
                                           end: end, title: manualTitle)
                }
            }
            .disabled(manualChannel.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                      || manualInvalidTime || dvr.busy)
        }
        .padding(12)
        .background(Palette.surface, in: RoundedRectangle(cornerRadius: 8))
    }

    private var manualInvalidTime: Bool {
        #if os(iOS)
        manualEnd <= manualStart
        #else
        false
        #endif
    }

    @ViewBuilder private func reminderRow(_ reminder: DvrReminder) -> some View {
        rowFrame {
            VStack(alignment: .leading, spacing: 2) {
                Text(reminder.title).font(LiveTvType.primary).lineLimit(1)
                Text("\(reminder.guideNumber) · \(liveTvTime(reminder.airingStart)) · \(reminder.state.rawValue)")
                    .font(LiveTvType.tertiary).foregroundStyle(Palette.muted)
            }
            Spacer(minLength: 8)
            DvrActionButton(label: "Remove") { Task { await dvr.forget(reminder) } }
        }
    }

    private func rowFrame<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        HStack(alignment: .center, spacing: 10) {
            content()
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.surface, in: RoundedRectangle(cornerRadius: 8))
    }
}

// MARK: - Permanent Recordings destination

/// Root navigation uses this page on both phone and television. The embedded
/// Live TV panel uses the same rows and actions, so moving between the two
/// destinations never creates a second controller or a second interpretation
/// of a recording state.
struct DvrRecordingsRootView: View {
    @EnvironmentObject private var model: AppModel
    @ObservedObject private var dvr = DvrController.shared
    @State private var now = Int(Date().timeIntervalSince1970)
    private let tick = Timer.publish(every: 30, on: .main, in: .common).autoconnect()

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if let label = dvr.indicatorLabel {
                NavigationLink {
                    DvrCaptureActivityView()
                } label: {
                    Label(label, systemImage: "record.circle.fill")
                        .font(LiveTvType.secondary)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Recording activity, \(label)")
            } else if dvr.overview?.fresh == true {
                Text("No active recordings")
                    .font(LiveTvType.secondary).foregroundStyle(Palette.muted)
            }
            if let error = dvr.overviewError, dvr.overview != nil {
                Text("Showing the last recording status · \(error)")
                    .font(LiveTvType.tertiary).foregroundStyle(Palette.muted)
            }
            DvrRecordingsPanel(dvr: dvr, now: now)
        }
        .padding()
        .background(Palette.bg)
        .navigationTitle("Recordings")
        .task { await dvr.load(origin: model.origin, token: Session.shared.token) }
        .onReceive(tick) { _ in now = Int(Date().timeIntervalSince1970) }
    }
}

struct DvrCaptureActivityView: View {
    @ObservedObject private var dvr = DvrController.shared
    @State private var now = Int(Date().timeIntervalSince1970)

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 10) {
                if let label = dvr.indicatorLabel {
                    Text(label).font(LiveTvType.title)
                } else if dvr.overview?.fresh == true {
                    Text("No active recordings").font(LiveTvType.title)
                } else {
                    Text("Status unavailable").font(LiveTvType.title)
                }
                if let age = dvr.overview?.observationAgeMs {
                    Text("DVR observed \(max(0, Int(age / 1_000))) seconds ago")
                        .font(LiveTvType.tertiary).foregroundStyle(Palette.muted)
                }
                ForEach(dvr.overview?.active ?? []) { row in
                    NavigationLink {
                        DvrRecordingDetailView(recordingId: row.id)
                    } label: {
                        VStack(alignment: .leading, spacing: 5) {
                            HStack {
                                Text(row.title).font(LiveTvType.primary).lineLimit(1)
                                Spacer()
                                Text(row.displayState).font(LiveTvType.secondary)
                            }
                            Text(activityDetail(row))
                                .font(LiveTvType.tertiary).foregroundStyle(Palette.muted)
                            LiveTvProgressLine(value: row.progress(now: now), height: 3)
                                .accessibilityLabel("Recording window elapsed")
                                .accessibilityValue("\(Int(row.progress(now: now) * 100)) percent")
                        }
                        .padding(12)
                        .background(Palette.surface, in: RoundedRectangle(cornerRadius: 8))
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding()
        }
        .background(Palette.bg)
        .navigationTitle("Recording activity")
        .task {
            while !Task.isCancelled {
                now = Int(Date().timeIntervalSince1970)
                try? await Task.sleep(for: .seconds(5))
            }
        }
    }

    private func activityDetail(_ row: DvrActiveRecording) -> String {
        var facts = [row.displayDetail]
        if let bytes = row.totalBytesWritten {
            facts.append(ByteCountFormatter.string(fromByteCount: Int64(clamping: bytes), countStyle: .file) + " written")
        }
        if let age = row.observation?.lastWriteAgeMs {
            facts.append("data written \(age / 1_000) s ago")
        }
        return facts.filter { !$0.isEmpty }.joined(separator: " · ")
    }
}

struct DvrRecordingDetailView: View {
    @ObservedObject private var dvr = DvrController.shared
    let recordingId: String

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                if let row = dvr.selectedRecording, row.id == recordingId {
                    Text(row.title).font(LiveTvType.title)
                    Text(detail(row)).font(LiveTvType.secondary).foregroundStyle(Palette.muted)
                    if let active = dvr.overview?.active.first(where: { $0.id == row.id }) {
                        Label(active.displayState, systemImage: "record.circle")
                        if !active.displayDetail.isEmpty {
                            Text(active.displayDetail).font(LiveTvType.secondary)
                        }
                    }
                    actionRow(row)
                    Divider()
                    Text("History").font(LiveTvType.primary)
                    if !dvr.selectedHistoryComplete {
                        Text(dvr.selectedHistoryTruncatedBefore == nil
                             ? "History starts when this version began collecting events."
                             : "Older history has expired under the retention limit.")
                            .font(LiveTvType.tertiary).foregroundStyle(Palette.muted)
                    }
                    ForEach(dvr.selectedEvents) { event in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(eventLabel(event.kind)).font(LiveTvType.secondary)
                            Text(eventDetail(event)).font(LiveTvType.tertiary).foregroundStyle(Palette.muted)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.vertical, 4)
                    }
                    if dvr.selectedEventsNext != nil {
                        DvrActionButton(label: "Load older history") {
                            Task { await dvr.loadOlderSelectedEvents() }
                        }
                    }
                } else {
                    ProgressView()
                }
                if let message = dvr.message {
                    Text(message).font(LiveTvType.tertiary).foregroundStyle(Palette.muted)
                }
            }
            .padding()
        }
        .background(Palette.bg)
        .navigationTitle("Recording details")
        .task(id: recordingId) {
            await dvr.selectRecording(recordingId)
            while !Task.isCancelled,
                  dvr.overview?.active.contains(where: { $0.id == recordingId }) == true {
                do { try await Task.sleep(for: .seconds(5)) } catch { return }
                await dvr.refreshSelectedEvents()
            }
        }
    }

    private func detail(_ row: DvrRecording) -> String {
        var facts = [
            "\(row.guideNumber) \(row.channelName)",
            "Programme \(liveTvTime(row.airingStart))–\(liveTvTime(row.airingEnd))",
            "Capture \(liveTvTime(row.captureStart))–\(liveTvTime(row.captureEnd))",
            row.state.label,
        ]
        if let reason = row.stateReason, !reason.isEmpty { facts.append(reason) }
        if row.state.hasMedia && row.itemId == nil { facts.append("preparing playback") }
        return facts.joined(separator: " · ")
    }

    @ViewBuilder private func actionRow(_ row: DvrRecording) -> some View {
        HStack {
            if let itemId = row.itemId, row.state.hasMedia {
                NavigationLink(value: Route.item(itemId)) { Text("Play") }
            }
            if row.state == .recording {
                DvrActionButton(label: row.stopping ? "Stopping…" : "Stop") {
                    Task { await dvr.delete(row) }
                }
            } else if row.state == .cancelled {
                DvrActionButton(label: "Restore") { Task { await dvr.restore(row) } }
            }
            if dvr.attention.contains(where: { $0.id == row.id }) {
                DvrActionButton(label: "Mark reviewed") {
                    Task { await dvr.acknowledgeSelectedAttention() }
                }
            }
        }
    }

    private func eventLabel(_ kind: String) -> String {
        kind.split(separator: "_").map { $0.capitalized }.joined(separator: " ")
    }

    private func eventDetail(_ event: DvrEvent) -> String {
        var facts = [Date(timeIntervalSince1970: Double(event.occurredAtMs) / 1_000).formatted()]
        if let attempt = event.attempt { facts.append("attempt \(attempt)") }
        if let reason = event.reasonCode { facts.append(reason.replacingOccurrences(of: "_", with: " ")) }
        return facts.joined(separator: " · ")
    }
}
