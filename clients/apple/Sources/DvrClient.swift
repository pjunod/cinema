import Foundation

// Recording, the schedule, rules and reminders, as the client sees them.
//
// Every shape below is the server's row verbatim. A client that re-derived a
// recording's title from the guide would change it under the person who
// scheduled it — the row copied the programme once, at the moment Record was
// pressed, and that copy is what this file decodes and draws.
//
// The reason these are `Decodable` rather than `Codable`: nothing here is ever
// sent back. Writes are the small request bodies at the bottom of the file,
// spelled in the server's own snake_case, exactly as `LiveTvSettingsChange`
// spells a settings write.

// MARK: - Wire contract

enum DvrOrigin: String, Decodable, Sendable {
    case manual
    case rule
}

/// Where an airing is in its life. `cancelled` is the only state a person
/// writes; everything from `recording` onward is written by the owner node
/// alone, which is why the client never invents one locally.
enum DvrState: String, Decodable, Sendable, CaseIterable {
    case scheduled
    case conflict
    case withdrawn
    case stale
    case recording
    case done
    case partial
    case failed
    case missed
    case cancelled
    case deleted

    /// Waiting for its moment: the schedule still plans a tuner for it.
    var pending: Bool { [.scheduled, .conflict, .withdrawn, .stale].contains(self) }

    /// A capture that produced a file the library can hold.
    var hasMedia: Bool { self == .done || self == .partial }

    var label: String {
        switch self {
        case .scheduled: return "Scheduled"
        case .conflict: return "No tuner free"
        case .withdrawn: return "Withdrawn"
        case .stale: return "Programme moved"
        case .recording: return "Recording"
        case .done: return "Recorded"
        case .partial: return "Recorded with a gap"
        case .failed: return "Failed"
        case .missed: return "Missed"
        case .cancelled: return "Skipped"
        case .deleted: return "Deleted"
        }
    }
}

enum DvrMatchMode: String, Decodable, Sendable {
    case seriesId = "series_id"
    case title

    /// A rule editor that implied an exactness the rule does not have would be
    /// worse than no description at all, so the two modes are named apart.
    var label: String { self == .seriesId ? "Series match" : "Title match" }
}

enum DvrKeepMode: String, Decodable, Sendable {
    case all
    case lastN = "last_n"
    case untilWatched = "until_watched"
    case days

    func label(value: Int) -> String {
        switch self {
        case .all: return "Keep all"
        case .lastN: return "Keep the last \(value)"
        case .untilWatched: return "Keep until watched"
        case .days: return "Keep for \(value) days"
        }
    }
}

enum DvrReminderState: String, Decodable, Sendable {
    case armed
    case fired
    case acked
    case expired
    /// The airing this reminder named is no longer in the guide at that time.
    case moved
}

struct DvrRecording: Decodable, Identifiable, Equatable, Sendable {
    let id: String
    let origin: DvrOrigin
    let ruleId: String?
    let requestedByUserId: Int?
    let channelId: String
    let guideNumber: String
    let channelName: String
    let airingStart: Int
    let airingEnd: Int
    let captureStart: Int
    let captureEnd: Int
    let title: String
    let episodeTitle: String?
    let episode: String?
    let synopsis: String?
    let imageUrl: String?
    let originalAirDate: String?
    let seriesId: String?
    let programmeId: String?
    let state: DvrState
    /// One sentence, for every state that is not plainly `scheduled`. It is
    /// what the schedule list shows under a conflicted or missed row.
    let stateReason: String?
    let attempt: Int
    let gapS: Int
    let lateStartS: Int
    let tunerOwnerNodeId: String?
    let path: String?
    let bytes: Int64
    let lastProgressMs: Int64?
    let stopRequestedAtMs: Int64?
    let stopRequestedByUserId: Int?
    let itemId: Int?
    let fileId: Int?
    let startedAtMs: Int64?
    let finishedAtMs: Int64?
    let stoppedByUserId: Int?
    /// Public attention rows carry the fact without exposing a user id.
    let stoppedEarly: Bool?
    let createdAtMs: Int64
    let updatedAtMs: Int64

    /// How far through its capture window a running recording is.
    func progress(now: Int) -> Double {
        let span = captureEnd - captureStart
        guard span > 0 else { return 0 }
        return min(max(Double(now - captureStart) / Double(span), 0), 1)
    }

    /// A stop the owner node has been asked for but has not yet consumed. The
    /// row stays `recording` until its next tick closes the file, so the list
    /// says "stopping" rather than pretending the capture is already over.
    var stopping: Bool { state == .recording && stopRequestedAtMs != nil }
}

struct DvrSchedule: Decodable, Sendable {
    /// How many rows have no tuner. The number the Scheduled chip carries, so
    /// a viewer learns about a clash without opening the list.
    let conflicts: Int
    let rows: [DvrRecording]
    let next: String?
}

struct DvrRecordingsPage: Decodable, Sendable {
    let rows: [DvrRecording]
    let next: String?
}

/// One fresh sample from the recorder owner. `phase` deliberately remains a
/// string: a newer owner may add a phase before this client ships, and an
/// unknown observation must render as unknown rather than make the complete
/// overview fail to decode.
struct DvrCaptureObservation: Decodable, Equatable, Sendable {
    let recordingId: String
    let channelId: String
    let airingStart: Int
    let ownerNodeId: String
    let configGeneration: Int64
    let servingGeneration: UInt64
    let attempt: Int
    let phase: String
    let observationAgeMs: UInt64
    let lastWriteAgeMs: UInt64?
    let firstWriteAtMs: Int64?
    let attemptBytesWritten: UInt64
    let priorAttemptBytes: UInt64?
    let writeBps: UInt64?
    let reasonCode: String?
}

struct DvrOverviewCounts: Decodable, Equatable, Sendable {
    let recording: Int?
    let starting: Int?
    let reconnecting: Int?
    let finishing: Int?
    let unconfirmed: Int?
    let attention: Int?

    var active: Int {
        [recording, starting, reconnecting, finishing, unconfirmed]
            .compactMap { $0 }.reduce(0, +)
    }
}

struct DvrOverviewDiagnostics: Decodable, Equatable, Sendable {
    let ownerNodeId: String
    let recordingSinks: Int?
    let recordingTransports: Int?
    let storageFreeBytes: UInt64?
}

struct DvrActiveRecording: Decodable, Identifiable, Equatable, Sendable {
    var id: String { recordingId }
    let recordingId: String
    let channelId: String
    let airingStart: Int
    let title: String
    let episodeTitle: String?
    let guideNumber: String
    let channelName: String
    let durableState: DvrState
    let stateReason: String?
    let airingEnd: Int
    let captureStart: Int
    let captureEnd: Int
    let stopRequestedAtMs: Int64?
    let lastConfirmedBytes: UInt64?
    let totalBytesWritten: UInt64?
    let observation: DvrCaptureObservation?
    let displayState: String
    let displayDetail: String
    let canStop: Bool
    let canSkip: Bool
    let canRestore: Bool
    let canDelete: Bool
    let canEditRule: Bool
    let canReorderRules: Bool
    let canViewDiagnostics: Bool

    func progress(now: Int) -> Double {
        let span = captureEnd - captureStart
        guard span > 0 else { return 0 }
        return min(max(Double(now - captureStart) / Double(span), 0), 1)
    }
}

struct DvrOverview: Decodable, Equatable, Sendable {
    let version: Int
    let serverNowMs: Int64
    let availability: String
    let runtimeSupported: Bool
    let observationAgeMs: UInt64?
    let counts: DvrOverviewCounts
    let activeTotal: Int?
    let activeTruncated: Bool
    let active: [DvrActiveRecording]
    let nextCaptureStart: Int?
    let diagnostics: DvrOverviewDiagnostics?

    func isFresh(clientAgeMs: UInt64 = 0) -> Bool {
        let age = (observationAgeMs ?? 0).addingReportingOverflow(clientAgeMs)
        guard availability != "unavailable", !age.overflow, age.partialValue <= 20_000 else {
            return false
        }
        return availability == "complete" || observationAgeMs != nil
    }

    func indicatorText(clientAgeMs: UInt64 = 0) -> String? {
        guard isFresh(clientAgeMs: clientAgeMs) else {
            return counts.active > 0 ? "Status unavailable" : nil
        }
        var parts: [String] = []
        for (count, label) in [
            (counts.recording, "recording"),
            (counts.starting, "starting"),
            (counts.reconnecting, "reconnecting"),
            (counts.finishing, "finishing"),
            (counts.unconfirmed, "unconfirmed"),
        ] {
            if let count, count > 0 { parts.append("\(count) \(label)") }
        }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }
}

struct DvrEvent: Decodable, Identifiable, Equatable, Sendable {
    var id: String { eventId }
    let recordingId: String
    let sequence: Int64
    let eventId: String
    let kind: String
    let occurredAtMs: Int64
    let attempt: Int64?
    let actorUserId: Int64?
    let reasonCode: String?
    // Facts are intentionally ignored by this first native presentation.
    // JSONDecoder ignores the additive object, including values whose types a
    // client cannot predict, while the typed fields above remain available.
}

struct DvrEventsPage: Decodable, Sendable {
    let rows: [DvrEvent]
    let next: String?
    let historyComplete: Bool
    let truncatedBeforeSequence: Int64?
}

struct DvrAttentionProjection: Decodable, Identifiable, Sendable {
    var id: String { recording.id }
    let recording: DvrRecording
    let latestAttentionSequence: Int64
    let latestAttentionAtMs: Int64
    let acknowledgedThroughSequence: Int64
}

struct DvrAttentionPage: Decodable, Sendable {
    let rows: [DvrAttentionProjection]
    let next: String?
    let total: Int
}

struct DvrAttentionAck: Decodable, Sendable {
    let recordingId: String
    let throughSequence: Int64
}

struct DvrSlots: Decodable, Sendable {
    let max: Int
    let reserve: Int
    let recording: Int
}

struct DvrStatus: Decodable, Sendable {
    let enabled: Bool
    let ownerNodeId: String
    let root: String
    /// Absent on a node that is not the tuner owner, which is ordinary rather
    /// than an error.
    let freeBytes: Int64?
    let floorBytes: Int64
    let slots: DvrSlots
    let nextStart: Int?
    let padStartS: Int
    let padEndS: Int
    let reminderLeadS: Int
}

struct DvrRule: Decodable, Identifiable, Equatable, Sendable {
    let id: String
    let ownerUserId: Int
    /// Lower wins, and unique across the server: the scheduler's answer to
    /// "which of these two airings gets the tuner" is total, so the list draws
    /// the rules in exactly the order that decides it.
    let priority: Int
    let name: String
    let matchMode: DvrMatchMode
    let matchValue: String
    /// `nil` matches any channel.
    let channelId: String?
    let newOnly: Bool
    let keepMode: DvrKeepMode
    let keepValue: Int
    let padStartS: Int
    let padEndS: Int
    let enabled: Bool
    let createdAtMs: Int64
    let updatedAtMs: Int64
}

struct DvrReminder: Decodable, Identifiable, Equatable, Sendable {
    let id: String
    let userId: Int
    let channelId: String
    let guideNumber: String
    let airingStart: Int
    let airingEnd: Int
    let title: String
    let leadS: Int
    let state: DvrReminderState
    let firedAtMs: Int64?
    let ackedAtMs: Int64?
    let createdAtMs: Int64
    let updatedAtMs: Int64
    /// Only the list route says whether a recording already covers this
    /// airing; the row a POST answers with does not, which is why this is
    /// optional rather than defaulted to a false the server never said.
    let coveredByRecording: Bool?

    var covered: Bool { coveredByRecording ?? false }

    /// When the overlay and the local notification are due.
    var fireAt: Int { airingStart - leadS }
}

/// What `DELETE /dvr/recordings/{id}` did. One verb, two answers: a planned
/// airing is gone when the call returns, and a running capture has only been
/// asked to stop — the owner's next tick closes the file.
enum DvrDeleteOutcome: Equatable, Sendable {
    case removed
    case stopping(requestedAt: Int64)
}

private struct DvrStopPending: Decodable {
    let pending: Bool
    let requestedAt: Int64
}

struct DvrFailure: Error, LocalizedError, Sendable {
    let code: String
    var errorDescription: String? {
        switch code {
        case "dvr_disabled": return "Recording is off. An administrator can enable it in Settings → Developer."
        case "airing_unknown": return "The guide no longer has that programme at that time. Refresh the guide and try again."
        case "airing_past": return "That programme has already finished, or has less than a minute left."
        case "rule_limit": return "This server already has the maximum number of recording rules."
        case "reminder_limit": return "You already have the maximum number of reminders set."
        case "delete_file_required": return "This recording has a file. Confirm to delete the file as well."
        case "admin_required": return "Only an administrator can change the order of recording rules."
        case "not_found": return "That recording is no longer on the server. Refresh the list."
        default: return "The server could not answer that recording request. Check the server and its network connection."
        }
    }
}

// MARK: - Writes

/// The rule edits this client offers, as separate shapes so no write can mix
/// two decisions. Spelled the same way `LiveTvSettingsChange` spells a
/// settings write: one flat object in the server's own snake_case.
enum DvrRuleChange {
    case enabled(Bool)
    case newOnly(Bool)

    func body() throws -> Data {
        var fields: [String: Any] = [:]
        switch self {
        case let .enabled(enabled): fields["enabled"] = enabled
        case let .newOnly(newOnly): fields["new_only"] = newOnly
        }
        return try JSONSerialization.data(withJSONObject: fields)
    }
}

// MARK: - Client

/// One authenticated profile's DVR. Every route here carries the account
/// bearer and nothing else, and redirects are refused for the same reason they
/// are on the Live TV surface: a redirect must not carry the account token
/// somewhere the viewer never named.
final class DvrAPI: @unchecked Sendable {
    let origin: String
    private let token: String?
    private let control: URLSession
    private let redirects = LiveTvNoRedirects()

    init(origin: String, token: String?, session: URLSession? = nil) {
        self.origin = origin
        self.token = token
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 15
        configuration.timeoutIntervalForResource = 15
        control = session ?? URLSession(configuration: configuration, delegate: redirects,
                                        delegateQueue: nil)
    }

    deinit {
        control.invalidateAndCancel()
    }

    /// Returns the status code beside the body, because two DVR answers are
    /// the status code: a stop request is `202` with the same shape a failure
    /// would have, and a second Record on one airing is `200` rather than
    /// `201` without either being an error.
    private func request(_ path: String, method: String = "GET",
                         body: Data? = nil, timeout: TimeInterval? = nil) async throws -> (Data, Int) {
        guard let base = Session.canonicalOrigin(origin),
              let url = URL(string: base + "/api/v1/" + path)
        else { throw DvrFailure(code: "dvr_unavailable") }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.httpBody = body
        if let timeout { request.timeoutInterval = timeout }
        if body != nil { request.setValue("application/json", forHTTPHeaderField: "Content-Type") }
        if let token { request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        let (data, response) = try await control.data(for: request)
        guard let response = response as? HTTPURLResponse else { throw DvrFailure(code: "dvr_unavailable") }
        guard (200..<300).contains(response.statusCode) else {
            struct WireFailure: Decodable { let code: String }
            if let failure = try? JSONDecoder().decode(WireFailure.self, from: data) {
                throw DvrFailure(code: failure.code)
            }
            // A reminder or a cancel that is already gone is the outcome the
            // caller wanted, not a failure to report to the viewer.
            if method == "DELETE", response.statusCode == 404 || response.statusCode == 410 {
                return (Data(), StatusCode.noContent)
            }
            if response.statusCode == 401 || response.statusCode == 403 {
                throw DvrFailure(code: "admin_required")
            }
            if response.statusCode == 404 { throw DvrFailure(code: "not_found") }
            throw DvrFailure(code: "dvr_unavailable")
        }
        return (data, response.statusCode)
    }

    private enum StatusCode {
        static let noContent = 204
        static let accepted = 202
    }

    private func decode<T: Decodable>(_ type: T.Type, data: Data) throws -> T {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(type, from: data)
    }

    func status() async throws -> DvrStatus {
        try decode(DvrStatus.self, data: await request("dvr/status").0)
    }

    func overview() async throws -> DvrOverview {
        try decode(DvrOverview.self, data: await request("dvr/overview", timeout: 4).0)
    }

    /// The plan, and with it the conflict count the Scheduled chip carries.
    /// Cancelled rows stay visible on request: a viewer who skipped the wrong
    /// episode has to be able to find it again to restore it.
    func schedule(from: Int, to: Int, after: String? = nil) async throws -> DvrSchedule {
        var components = URLComponents()
        components.queryItems = [
            URLQueryItem(name: "from", value: String(from)),
            URLQueryItem(name: "to", value: String(to)),
            URLQueryItem(name: "limit", value: "100"),
        ]
        if let after { components.queryItems?.append(URLQueryItem(name: "after", value: after)) }
        let query = components.percentEncodedQuery.map { "?" + $0 } ?? ""
        return try decode(DvrSchedule.self, data: await request("dvr/schedule" + query).0)
    }

    func recordingsPage(states: [DvrState], after: String? = nil) async throws -> DvrRecordingsPage {
        var query: [URLQueryItem] = states.isEmpty
            ? []
            : [URLQueryItem(name: "state", value: states.map(\.rawValue).joined(separator: ","))]
        if let after { query.append(URLQueryItem(name: "after", value: after)) }
        var components = URLComponents()
        components.queryItems = query
        return try decode(DvrRecordingsPage.self,
                          data: await request("dvr/recordings" + (components.percentEncodedQuery.map { "?" + $0 } ?? "")).0)
    }

    func recordings(states: [DvrState]) async throws -> [DvrRecording] {
        try await recordingsPage(states: states).rows
    }

    func recording(_ id: String) async throws -> DvrRecording {
        try decode(DvrRecording.self, data: await request(
            "dvr/recordings/" + LiveTvAPI.pathComponent(id)).0)
    }

    func events(_ id: String, before: String? = nil,
                after: String? = nil, limit: Int = 50) async throws -> DvrEventsPage {
        var query = [URLQueryItem(name: "limit", value: String(max(1, min(100, limit))))]
        if let before { query.append(URLQueryItem(name: "before", value: before)) }
        if let after { query.append(URLQueryItem(name: "after", value: after)) }
        var components = URLComponents()
        components.queryItems = query
        let suffix = components.percentEncodedQuery.map { "?" + $0 } ?? ""
        return try decode(DvrEventsPage.self, data: await request(
            "dvr/recordings/" + LiveTvAPI.pathComponent(id) + "/events" + suffix).0)
    }

    func attention(after: String? = nil) async throws -> DvrAttentionPage {
        let suffix = after.map {
            var components = URLComponents()
            components.queryItems = [URLQueryItem(name: "after", value: $0)]
            return components.percentEncodedQuery.map { "?" + $0 } ?? ""
        } ?? ""
        return try decode(DvrAttentionPage.self,
                          data: await request("dvr/attention" + suffix).0)
    }

    func acknowledgeAttention(_ id: String, through sequence: Int64) async throws -> DvrAttentionAck {
        let body = try JSONSerialization.data(withJSONObject: ["through_sequence": sequence])
        return try decode(DvrAttentionAck.self, data: await request(
            "dvr/recordings/" + LiveTvAPI.pathComponent(id) + "/attention/ack",
            method: "POST", body: body).0)
    }

    /// Record one airing. Two people pressing Record on the same cell both
    /// wanted the same thing and both get it: the server answers `200` with
    /// the existing row rather than refusing the second.
    func record(channelId: String, airingStart: Int) async throws -> DvrRecording {
        let fields: [String: Any] = ["channel_id": channelId, "airing_start": airingStart]
        let body = try JSONSerialization.data(withJSONObject: fields)
        return try decode(DvrRecording.self,
                          data: await request("dvr/recordings", method: "POST", body: body).0)
    }

    func record(channelId: String, captureStart: Int, captureEnd: Int,
                title: String?) async throws -> DvrRecording {
        var fields: [String: Any] = [
            "channel_id": channelId,
            "capture_start": captureStart,
            "capture_end": captureEnd,
        ]
        if let title, !title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            fields["title"] = title
        }
        let body = try JSONSerialization.data(withJSONObject: fields)
        return try decode(DvrRecording.self,
                          data: await request("dvr/recordings", method: "POST", body: body).0)
    }

    func delete(_ id: String, deleteFile: Bool = false) async throws -> DvrDeleteOutcome {
        let path = "dvr/recordings/" + LiveTvAPI.pathComponent(id) + (deleteFile ? "?delete_file=1" : "")
        let (data, status) = try await request(path, method: "DELETE")
        guard status == StatusCode.accepted,
              let pending = try? decode(DvrStopPending.self, data: data), pending.pending
        else { return .removed }
        return .stopping(requestedAt: pending.requestedAt)
    }

    func restore(_ id: String) async throws -> DvrRecording {
        try decode(DvrRecording.self, data: await request(
            "dvr/recordings/" + LiveTvAPI.pathComponent(id) + "/restore", method: "POST").0)
    }

    func rules() async throws -> [DvrRule] {
        try decode([DvrRule].self, data: await request("dvr/rules").0)
    }

    /// "Record series" in two presses rather than a form: the server fills the
    /// mode, the value and the channel from the guide cell.
    func createRule(channelId: String, airingStart: Int) async throws -> DvrRule {
        let airing: [String: Any] = ["channel_id": channelId, "airing_start": airingStart]
        let body = try JSONSerialization.data(withJSONObject: ["from_airing": airing])
        return try decode(DvrRule.self, data: await request("dvr/rules", method: "POST", body: body).0)
    }

    func updateRule(_ id: String, _ change: DvrRuleChange) async throws -> DvrRule {
        try decode(DvrRule.self, data: await request(
            "dvr/rules/" + LiveTvAPI.pathComponent(id), method: "PUT", body: change.body()).0)
    }

    func deleteRule(_ id: String) async throws {
        _ = try await request("dvr/rules/" + LiveTvAPI.pathComponent(id), method: "DELETE")
    }

    /// Admin only. The order decides who gets a tuner when two rules want one,
    /// so it is a server-wide decision rather than a per-viewer preference.
    func reorderRules(_ ids: [String]) async throws -> [DvrRule] {
        let body = try JSONSerialization.data(withJSONObject: ["ids": ids])
        return try decode([DvrRule].self,
                          data: await request("dvr/rules/order", method: "PUT", body: body).0)
    }

    func reminders(due: Bool = false) async throws -> [DvrReminder] {
        try decode([DvrReminder].self,
                   data: await request("dvr/reminders" + (due ? "?due=1" : "")).0)
    }

    func remind(channelId: String, airingStart: Int, leadS: Int? = nil) async throws -> DvrReminder {
        var fields: [String: Any] = ["channel_id": channelId, "airing_start": airingStart]
        if let leadS { fields["lead_s"] = leadS }
        let body = try JSONSerialization.data(withJSONObject: fields)
        return try decode(DvrReminder.self,
                          data: await request("dvr/reminders", method: "POST", body: body).0)
    }

    func deleteReminder(_ id: String) async throws {
        _ = try await request("dvr/reminders/" + LiveTvAPI.pathComponent(id), method: "DELETE")
    }

    /// Acknowledged on one device, gone from every other.
    func ackReminder(_ id: String) async throws {
        _ = try await request("dvr/reminders/" + LiveTvAPI.pathComponent(id) + "/ack", method: "POST")
    }
}

// MARK: - Cell marks

/// What one guide cell draws in its corner.
///
/// An airing gets one mark, never two: a recording is a stronger promise than
/// a reminder, so a cell that is both says `REC` rather than ringing a bell at
/// someone whose programme is already being kept.
enum DvrCellMark: Equatable, Sendable {
    /// Two dots when a rule put it there, so "this episode" and "this series"
    /// are distinguishable without opening anything.
    case scheduled(series: Bool)
    /// Carries the capture window rather than a fraction, so the underline
    /// advances on the page's own clock instead of needing another fetch.
    case recording(captureStart: Int, captureEnd: Int)
    case conflict
    /// A rule row no rule matches any more, or a manual row whose programme
    /// moved. Hollow: it is planned by nobody, but nobody cancelled it either.
    case lapsed
    case reminder

    /// Read aloud beside the cell's own label, because a coloured dot in a
    /// corner is exactly the kind of state VoiceOver otherwise loses.
    var accessibilityDescription: String {
        switch self {
        case .scheduled(let series): return series ? "recording this series" : "recording"
        case .recording: return "recording now"
        case .conflict: return "no tuner free"
        case .lapsed: return "no longer planned"
        case .reminder: return "reminder set"
        }
    }
}

/// The marks for one guide load, keyed the way an airing is identified
/// everywhere else in this feature: `(channel, start)`, for its whole life.
struct DvrMarks: Equatable, Sendable {
    private struct Airing: Hashable {
        let channelId: String
        let airingStart: Int
    }

    private var marks: [Airing: DvrCellMark] = [:]

    init() {}

    init(schedule: [DvrRecording], reminders: [DvrReminder]) {
        for reminder in reminders where reminder.state == .armed || reminder.state == .fired {
            marks[Airing(channelId: reminder.channelId, airingStart: reminder.airingStart)] = .reminder
        }
        for row in schedule {
            let mark: DvrCellMark? = switch row.state {
            case .scheduled: .scheduled(series: row.ruleId != nil)
            case .recording: .recording(captureStart: row.captureStart, captureEnd: row.captureEnd)
            case .conflict: .conflict
            case .withdrawn, .stale: .lapsed
            // A cancelled row is in the schedule so it can be restored from
            // the list; on the grid it is a decision already taken, and a mark
            // would say the opposite of what the viewer asked for.
            default: nil
            }
            guard let mark else { continue }
            marks[Airing(channelId: row.channelId, airingStart: row.airingStart)] = mark
        }
    }

    func mark(channelId: String, airingStart: Int) -> DvrCellMark? {
        marks[Airing(channelId: channelId, airingStart: airingStart)]
    }

    var isEmpty: Bool { marks.isEmpty }
}
