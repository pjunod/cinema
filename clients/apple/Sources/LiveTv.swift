import Foundation

struct LiveTvChannel: Codable, Identifiable, Equatable, Sendable {
    let id: String
    let guideNumber: String
    let guideName: String
    let favorite: Bool
    let drm: Bool
    let support: String
    var watchable: Bool { !drm && support == "ready" }
    var title: String { "\(guideNumber) · \(guideName)" }
}

struct LiveTvLineup: Decodable, Sendable {
    let channels: [LiveTvChannel]
    let freshness: String
    let ageSeconds: Int
}

struct LiveTvStarted: Decodable, Sendable {
    let sessionId: String
    let channel: LiveTvChannel
    let live: Bool
}

struct LiveTvStatus: Decodable, Sendable {
    let state: String
    let ownerNodeId: String
    let encoder: String
    let outputHeight: Int
}

struct LiveTvSettings: Decodable, Equatable, Sendable {
    let liveTvEnabled: Bool
    let liveTvDeviceIpv4: String
    let liveTvOwnerNodeId: String
    let liveTvMaxSessions: Int
    let liveTvOutputHeight: Int
    let liveTvConfigGeneration: Int64
    let liveTvTransitionFromOwnerNodeId: String
    let liveTvTransitionDrainBefore: Int64
}

struct LiveTvReadiness: Decodable, Sendable {
    struct Check: Decodable, Identifiable, Sendable {
        let id: String
        let ready: Bool
        let message: String
    }
    let ready: Bool
    let generation: Int64
    let checks: [Check]
}

/// The three write shapes deliberately cannot mix configuration, enablement,
/// or physical recovery. Every mutation carries the last observed generation.
enum LiveTvSettingsChange {
    case configure(ipv4: String, owner: String, sessions: Int, height: Int)
    case enabled(Bool)
    case fencedOwner(owner: String, cutoff: Int64)

    func body(generation: Int64) throws -> Data {
        var fields: [String: Any] = ["live_tv_config_generation": generation]
        switch self {
        case let .configure(ipv4, owner, sessions, height):
            fields["live_tv_device_ipv4"] = ipv4
            fields["live_tv_owner_node_id"] = owner
            fields["live_tv_max_sessions"] = sessions
            fields["live_tv_output_height"] = height
        case let .enabled(enabled): fields["live_tv_enabled"] = enabled
        case let .fencedOwner(owner, cutoff):
            fields["live_tv_fenced_owner"] = [
                "owner_node_id": owner, "drain_before_generation": cutoff,
                "stopped_and_restart_prevented": true,
            ]
        }
        return try JSONSerialization.data(withJSONObject: fields)
    }
}

struct LiveTvFailure: Error, LocalizedError, Sendable {
    let code: String
    var errorDescription: String? {
        switch code {
        case "live_tv_disabled": return "Live TV is off. An administrator can enable it in Settings → Developer."
        case "live_tv_protocol_unready": return "Live TV is waiting for every serving node to run a compatible version."
        case "tuner_capacity": return "All Live TV slots are busy. Close another session and try again."
        case "tuner_unavailable": return "The tuner cannot start this channel. Check reception and other tuner clients."
        case "channel_not_found": return "This channel is no longer available. Refresh the lineup."
        case "drm_unsupported": return "DRM-protected television is not supported."
        case "codec_unsupported": return "The tuner owner or this device cannot decode this channel. ATSC 3.0 may require HEVC and AC-4 support."
        case "startup_timeout": return "The channel did not produce a live segment before the startup deadline."
        case "stream_failed": return "The live stream stopped. Select a channel to try again."
        case "capability_expired": return "The live session expired. Select a channel to start again."
        case "settings_conflict": return "Live TV settings changed elsewhere. Reload before trying again."
        case "start_outcome_unknown": return "The start response was lost. Wait 90 seconds for any unclaimed tuner session to expire before trying again."
        case "live_tv_storage_unavailable": return "Live TV cannot save its restart-safety marker. Check this device's available app storage before starting a channel."
        case "admin_required": return "Only an administrator can change Live TV settings."
        default: return "The tuner owner is unavailable. Check the server and its network connection."
        }
    }
}

protocol LiveTvRequests {
    func start(_ channel: String) async throws -> LiveTvStarted
    func release(_ capability: String) async throws
}

/// Captures one authenticated profile. Narrow media/control capabilities never
/// carry the account token, and HTTP redirects cannot carry either elsewhere.
final class LiveTvAPI: LiveTvRequests, @unchecked Sendable {
    let origin: String
    private let token: String?
    private let transport: URLSession
    private let control: URLSession
    private let cleanup: URLSession
    private let redirects = LiveTvNoRedirects()

    init(origin: String, token: String?, session: URLSession? = nil) {
        self.origin = origin
        self.token = token
        func makeSession(_ timeout: TimeInterval, delegate: URLSessionTaskDelegate) -> URLSession {
            let configuration = URLSessionConfiguration.ephemeral
            configuration.timeoutIntervalForRequest = timeout
            configuration.timeoutIntervalForResource = timeout
            return URLSession(configuration: configuration, delegate: delegate, delegateQueue: nil)
        }
        transport = session ?? makeSession(45, delegate: redirects)
        control = session ?? makeSession(15, delegate: redirects)
        cleanup = session ?? makeSession(8, delegate: redirects)
    }

    deinit {
        transport.invalidateAndCancel()
        control.invalidateAndCancel()
        cleanup.invalidateAndCancel()
    }

    static func pathComponent(_ value: String) -> String {
        value.addingPercentEncoding(withAllowedCharacters: .alphanumerics) ?? ""
    }

    func playlistURL(_ capability: String) throws -> URL {
        guard !capability.isEmpty, capability.utf8.count <= 1024,
              let base = Session.canonicalOrigin(origin),
              let url = URL(string: base + "/api/v1/live-tv/sessions/" + Self.pathComponent(capability) + "/index.m3u8")
        else { throw LiveTvFailure(code: "capability_expired") }
        return url
    }

    private func request(_ path: String, method: String = "GET", authenticated: Bool = false,
                         body: Data? = nil, session: URLSession? = nil) async throws -> Data {
        guard let base = Session.canonicalOrigin(origin), let url = URL(string: base + "/api/v1/" + path)
        else { throw LiveTvFailure(code: "owner_unavailable") }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.httpBody = body
        if body != nil { request.setValue("application/json", forHTTPHeaderField: "Content-Type") }
        if authenticated, let token { request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        let (data, response) = try await (session ?? control).data(for: request)
        guard let response = response as? HTTPURLResponse else { throw LiveTvFailure(code: "owner_unavailable") }
        guard (200..<300).contains(response.statusCode) else {
            struct WireFailure: Decodable { let code: String }
            if let failure = try? JSONDecoder().decode(WireFailure.self, from: data) { throw LiveTvFailure(code: failure.code) }
            if method == "DELETE", response.statusCode == 404 || response.statusCode == 410 { return Data() }
            if response.statusCode == 401 || response.statusCode == 403 { throw LiveTvFailure(code: "admin_required") }
            // An untyped intermediary failure on POST has no reliable start outcome.
            if method == "POST", path.hasSuffix("/sessions") { throw LiveTvFailure(code: "start_outcome_unknown") }
            throw LiveTvFailure(code: "owner_unavailable")
        }
        return data
    }

    private func decode<T: Decodable>(_ type: T.Type, data: Data) throws -> T {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(type, from: data)
    }

    func lineup() async throws -> LiveTvLineup {
        try decode(LiveTvLineup.self, data: await request("live-tv/channels", authenticated: true, session: transport))
    }

    func start(_ channel: String) async throws -> LiveTvStarted {
        try decode(LiveTvStarted.self, data: await request("live-tv/channels/\(Self.pathComponent(channel))/sessions",
                                                       method: "POST", authenticated: true, session: transport))
    }

    func release(_ capability: String) async throws {
        for attempt in 0..<2 {
            do {
                _ = try await request("live-tv/sessions/\(Self.pathComponent(capability))", method: "DELETE", session: cleanup)
                return
            } catch let failure as LiveTvFailure where failure.code == "capability_expired" { return }
            catch { if attempt == 1 { throw error } }
        }
    }

    func keepalive(_ capability: String) async throws {
        _ = try await request("live-tv/sessions/\(Self.pathComponent(capability))/keepalive", method: "PUT")
    }

    func status(_ capability: String) async throws -> LiveTvStatus {
        try decode(LiveTvStatus.self, data: await request("live-tv/sessions/\(Self.pathComponent(capability))/status"))
    }

    func settings() async throws -> LiveTvSettings {
        try decode(LiveTvSettings.self, data: await request("settings", authenticated: true, session: transport))
    }

    func update(_ change: LiveTvSettingsChange, generation: Int64) async throws -> LiveTvSettings {
        try decode(LiveTvSettings.self, data: await request("settings", method: "PUT", authenticated: true,
                                                          body: change.body(generation: generation), session: transport))
    }

    func readiness() async throws -> LiveTvReadiness {
        try decode(LiveTvReadiness.self, data: await request("live-tv/readiness/refresh", method: "POST",
                                                           authenticated: true, session: transport))
    }
}

private final class LiveTvNoRedirects: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}

/// Serializes starts, including a late response after dismissal. Failed release
/// retains the single capability and blocks a new tuner until cleanup succeeds.
@MainActor
final class LiveTvStartBarrier {
    static let shared = LiveTvStartBarrier(persistence: LiveTvFileBarrierStore())
    private let now: () -> ContinuousClock.Instant
    private let persistence: LiveTvBarrierStore?
    private var storageFailed = false
    private var until: ContinuousClock.Instant?

    init(now: @escaping () -> ContinuousClock.Instant = { ContinuousClock.now }, persistence: LiveTvBarrierStore? = nil) {
        self.now = now
        self.persistence = persistence
        do { if try persistence?.pending() == true { until = now().advanced(by: .seconds(90)) } }
        catch { storageFailed = true }
    }
    var pending: Bool { until.map { now() < $0 } ?? false }

    func begin() throws {
        guard !storageFailed else { throw LiveTvFailure(code: "live_tv_storage_unavailable") }
        guard !pending else { throw LiveTvFailure(code: "start_outcome_unknown") }
        try arm() // persisted before dispatch, including a process killed mid-POST
    }

    func arm() throws {
        do { try persistence?.setPending(true) }
        catch { throw LiveTvFailure(code: "live_tv_storage_unavailable") }
        until = now().advanced(by: .seconds(90))
    }

    func confirm() {
        do { try persistence?.setPending(false); until = nil }
        catch { /* An uncleared marker conservatively waits again on restart. */ }
    }

    func acquired() {
        // Known ownership may be released immediately for a channel switch,
        // but its disk marker must survive a crash until DELETE is confirmed.
        until = nil
    }
}

protocol LiveTvBarrierStore {
    func pending() throws -> Bool
    func setPending(_ value: Bool) throws
}

/// Atomic one-byte marker, no token, capability, profile or device information.
/// A fresh app process waits a full monotonic grace period if it finds it.
private struct LiveTvFileBarrierStore: LiveTvBarrierStore {
    private func url() throws -> URL {
        try FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask,
                                    appropriateFor: nil, create: true).appendingPathComponent("live-tv-start.pending")
    }
    func pending() throws -> Bool { FileManager.default.fileExists(atPath: try url().path) }
    func setPending(_ value: Bool) throws {
        let file = try url()
        if value { try Data([1]).write(to: file, options: .atomic) }
        else if FileManager.default.fileExists(atPath: file.path) { try FileManager.default.removeItem(at: file) }
    }
}

struct LiveTvPlaybackWatchdog {
    private let now: () -> ContinuousClock.Instant
    private var lastProgress: ContinuousClock.Instant
    private var lastPosition = 0.0

    init(now: @escaping () -> ContinuousClock.Instant = { ContinuousClock.now }) {
        self.now = now
        lastProgress = now()
    }

    mutating func observe(position: Double) -> Bool {
        guard position.isFinite, position > lastPosition + 0.1 else { return false }
        lastPosition = position
        lastProgress = now()
        return true
    }

    var expired: Bool { lastProgress.duration(to: now()) >= .seconds(30) }
}

@MainActor
final class LiveTvLease {
    private let requests: LiveTvRequests
    private let barrier: LiveTvStartBarrier
    private var tail: Task<Void, Never>?
    private var generation = 0
    private(set) var current: LiveTvStarted?

    init(requests: LiveTvRequests, barrier: LiveTvStartBarrier? = nil) {
        self.requests = requests
        // App-wide, deliberately conservative: changing profile/server cannot
        // forget an unknown result. No account credentials live in the barrier.
        self.barrier = barrier ?? .shared
    }

    func start(_ channel: String) async throws -> LiveTvStarted? {
        generation += 1
        let expected = generation, previous = tail
        let operation = Task { @MainActor in
            await previous?.value
            try await releaseCurrent()
            guard generation == expected else { return nil as LiveTvStarted? }
            try barrier.begin()
            let info: LiveTvStarted
            do { info = try await requests.start(channel) }
            catch {
                let code = (error as? LiveTvFailure)?.code
                if code.map({ ["owner_unavailable", "startup_timeout", "stream_failed", "start_outcome_unknown"].contains($0) }) ?? true {
                    try? barrier.arm()
                    throw LiveTvFailure(code: "start_outcome_unknown")
                }
                barrier.confirm()
                throw error
            }
            guard !info.sessionId.isEmpty, info.sessionId.utf8.count <= 1024 else {
                try? barrier.arm()
                throw LiveTvFailure(code: "start_outcome_unknown")
            }
            current = info
            barrier.acquired()
            guard info.live, !info.sessionId.isEmpty, generation == expected else {
                try await releaseCurrent()
                return nil
            }
            return info
        }
        tail = Task { _ = try? await operation.value }
        return try await operation.value
    }

    func stop() async throws {
        generation += 1
        let previous = tail
        let operation = Task { @MainActor in
            await previous?.value
            try await releaseCurrent()
        }
        tail = Task { _ = try? await operation.value }
        try await operation.value
    }

    private func releaseCurrent() async throws {
        guard let info = current else { return }
        // Cleanup still runs when marker storage fails; abandoning a known
        // physical owner would be worse. Any later start must persist first.
        try? barrier.arm()
        try await requests.release(info.sessionId)
        current = nil
        barrier.confirm()
    }
}
