import Foundation

struct LiveTvChannel: Codable, Identifiable, Equatable, Sendable {
    let id: String
    let guideNumber: String
    let guideName: String
    let favorite: Bool
    let drm: Bool
    let support: String
    let hd: Bool?
    let videoCodec: String?
    let audioCodec: String?
    let sourceFormat: LiveTvSourceFormat?

    init(
        id: String,
        guideNumber: String,
        guideName: String,
        favorite: Bool,
        drm: Bool,
        support: String,
        hd: Bool?,
        videoCodec: String?,
        audioCodec: String?,
        sourceFormat: LiveTvSourceFormat? = nil
    ) {
        self.id = id
        self.guideNumber = guideNumber
        self.guideName = guideName
        self.favorite = favorite
        self.drm = drm
        self.support = support
        self.hd = hd
        self.videoCodec = videoCodec
        self.audioCodec = audioCodec
        self.sourceFormat = sourceFormat
    }

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        id = try values.decode(String.self, forKey: .id)
        guideNumber = try values.decode(String.self, forKey: .guideNumber)
        guideName = try values.decode(String.self, forKey: .guideName)
        favorite = try values.decodeIfPresent(Bool.self, forKey: .favorite) ?? false
        drm = try values.decodeIfPresent(Bool.self, forKey: .drm) ?? false
        support = try values.decodeIfPresent(String.self, forKey: .support) ?? "ready"
        hd = (try? values.decodeIfPresent(Bool.self, forKey: .hd)) ?? nil
        videoCodec = (try? values.decodeIfPresent(String.self, forKey: .videoCodec)) ?? nil
        audioCodec = (try? values.decodeIfPresent(String.self, forKey: .audioCodec)) ?? nil
        sourceFormat = (try? values.decodeIfPresent(LiveTvSourceFormat.self, forKey: .sourceFormat)) ?? nil
    }
    var watchable: Bool { !drm && support == "ready" }
    var title: String { "\(guideNumber) · \(guideName)" }
    var formatBadges: [String] {
        var badges = [String]()
        if let pictureClass { badges.append(pictureClass) }
        for value in [videoCodec, sourceAudioDescription] {
            guard let value = value?.trimmingCharacters(in: .whitespacesAndNewlines),
                  !value.isEmpty else { continue }
            let badge = value.uppercased()
            if !badges.contains(badge) { badges.append(badge) }
        }
        return badges
    }
    var sourceFormatDescription: String? {
        var facts = [String]()
        if let exactSourcePicture { facts.append(exactSourcePicture) }
        if let videoCodec = videoCodec?.trimmingCharacters(in: .whitespacesAndNewlines),
           !videoCodec.isEmpty { facts.append("\(videoCodec.uppercased()) video") }
        if let sourceAudioDescription { facts.append("\(sourceAudioDescription) audio") }
        return facts.isEmpty ? nil : facts.joined(separator: " · ")
    }

    var pictureClass: String? {
        if let height = sourceFormat?.videoHeight {
            if height > 2160 { return "4K+" }
            if height == 2160 { return "4K" }
            if height >= 720 { return "HD" }
            return "SD"
        }
        return hd.map { $0 ? "HD" : "SD" }
    }

    var exactSourcePicture: String? {
        guard let width = sourceFormat?.videoWidth,
              let height = sourceFormat?.videoHeight else { return pictureClass }
        let suffix = sourceFormat?.scan == "progressive" ? "p" : sourceFormat?.scan == "interlaced" ? "i" : ""
        return "\(width)×\(height)\(suffix)"
    }

    var sourceAudioDescription: String? {
        let codec = audioCodec?.trimmingCharacters(in: .whitespacesAndNewlines).uppercased()
        let layout: String? = switch sourceFormat?.audioLayout {
        case "mono": "Mono"
        case "stereo": "Stereo"
        case let value?: value
        case nil: sourceFormat?.audioChannels.map { "\($0) ch" }
        }
        let parts: [String] = [codec, layout].compactMap { $0 }.filter { !$0.isEmpty }
        return parts.isEmpty ? nil : parts.joined(separator: " ")
    }

    func removingSourceFormat() -> LiveTvChannel {
        LiveTvChannel(
            id: id,
            guideNumber: guideNumber,
            guideName: guideName,
            favorite: favorite,
            drm: drm,
            support: support,
            hd: hd,
            videoCodec: videoCodec,
            audioCodec: audioCodec,
            sourceFormat: nil
        )
    }
}

struct LiveTvSourceFormat: Codable, Equatable, Sendable {
    let videoWidth: Int?
    let videoHeight: Int?
    let scan: String?
    let audioChannels: Int?
    let audioLayout: String?
    let observedAt: Int64

    init(
        videoWidth: Int? = nil,
        videoHeight: Int? = nil,
        scan: String? = nil,
        audioChannels: Int? = nil,
        audioLayout: String? = nil,
        observedAt: Int64
    ) {
        self.videoWidth = videoWidth
        self.videoHeight = videoHeight
        self.scan = scan
        self.audioChannels = audioChannels
        self.audioLayout = audioLayout
        self.observedAt = observedAt
    }

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        let observedAt = try values.decode(Int64.self, forKey: .observedAt)
        guard observedAt > 0 else {
            throw DecodingError.dataCorruptedError(
                forKey: .observedAt,
                in: values,
                debugDescription: "source observation time must be positive"
            )
        }
        let dimension: (CodingKeys) -> Int? = { key in
            (try? values.decodeIfPresent(Int.self, forKey: key))
                .flatMap { (1...16_384).contains($0) ? $0 : nil }
        }
        videoWidth = dimension(.videoWidth)
        videoHeight = dimension(.videoHeight)
        let rawScan = try? values.decodeIfPresent(String.self, forKey: .scan)
        scan = ["progressive", "interlaced"].contains(rawScan ?? "") ? rawScan : nil
        let rawChannels = try? values.decodeIfPresent(Int.self, forKey: .audioChannels)
        audioChannels = rawChannels.flatMap { (1...32).contains($0) ? $0 : nil }
        let rawLayoutValue = (try? values.decodeIfPresent(String.self, forKey: .audioLayout)) ?? nil
        let rawLayout = rawLayoutValue?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        audioLayout = rawLayout.flatMap { value in
            guard !value.isEmpty, value.utf8.count <= 32,
                  value.allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber || ".+_- ".contains($0)) })
            else { return nil }
            return value
        }
        self.observedAt = observedAt
    }
}

enum TvLiveLayout: String, Codable, CaseIterable, Identifiable, Sendable {
    case guidePreview = "guide_preview"
    case guideOverlay = "guide_overlay"
    case channelBrowser = "channel_browser"

    var id: String { rawValue }
    var label: String {
        switch self {
        case .guidePreview: "Preview"
        case .guideOverlay: "Over picture"
        case .channelBrowser: "Preview"
        }
    }

    /// What the raw value renders as. `channel_browser` and `guide_preview`
    /// only ever differed in which browse view they opened with; now that On
    /// now is a list beside the picture in both, a third entry would be a
    /// choice with no consequence. The case is kept so an existing
    /// `@AppStorage` value still decodes and round-trips unchanged.
    var presented: TvLiveLayout { self == .channelBrowser ? .guidePreview : self }

    /// The entries the layout sheet offers.
    static var offered: [TvLiveLayout] { [.guidePreview, .guideOverlay] }
}

struct LiveTvLineup: Decodable, Sendable {
    let channels: [LiveTvChannel]
    let freshness: String
    let ageSeconds: Int
}

struct LiveTvRational: Codable, Sendable {
    let num: Int
    let den: Int
}

struct LiveTvHlsFormat: Codable, Sendable {
    let container: String
    let video: String
    let audio: String
}

struct LiveTvVideoLimit: Codable, Sendable {
    let codec: String
    let profile: String?
    let maxWidth: Int
    let maxHeight: Int
    let maxFrameRate: LiveTvRational
    let interlaced: Bool
}

struct LiveTvAudioLimit: Codable, Sendable {
    let codec: String
    let maxChannels: Int
}

struct LiveTvCompatibility: Codable, Sendable {
    let failedVideo: Bool
    let failedAudio: Bool
    let failedContainer: Bool
}

struct LiveTvPlaybackEnvelope: Encodable {
    let v = 1
    let caps: DeviceCaps
    let hlsFormats: [LiveTvHlsFormat]
    let videoLimits: [LiveTvVideoLimit]
    let audioLimits: [LiveTvAudioLimit]
    let maxHeight: Int?
    let maxBitrateBps: Int?
    let compatibility: LiveTvCompatibility?

    static func current(compatibility: LiveTvCompatibility? = nil) -> Self {
        let caps = Caps.capsDocument()
        let liveAudio = caps.audio.filter { ["aac", "ac3", "eac3"].contains($0) }
        var formats = [LiveTvHlsFormat(container: "mpegts", video: "h264", audio: "aac")]
        for video in caps.video {
            let container = video.codec == "hevc" ? "fmp4" : "mpegts"
            for audio in liveAudio {
                formats.append(LiveTvHlsFormat(container: container, video: video.codec, audio: audio))
            }
        }
        let limits = caps.video.flatMap { video -> [LiveTvVideoLimit] in
            let profiles = video.profiles?.isEmpty == false ? video.profiles!.map(Optional.some) : [nil]
            return profiles.map { profile in
                LiveTvVideoLimit(codec: video.codec, profile: profile, maxWidth: 3840,
                                 maxHeight: video.maxHeight ?? 2160,
                                 maxFrameRate: LiveTvRational(num: 60, den: 1), interlaced: false)
            }
        }
        return Self(
            caps: caps,
            hlsFormats: Array(Set(formats.map { "\($0.container)|\($0.video)|\($0.audio)" })).sorted().compactMap { value in
                let parts = value.split(separator: "|").map(String.init)
                return parts.count == 3 ? LiveTvHlsFormat(container: parts[0], video: parts[1], audio: parts[2]) : nil
            },
            videoLimits: limits,
            audioLimits: liveAudio.map { LiveTvAudioLimit(codec: $0, maxChannels: $0 == "aac" ? 2 : 8) },
            maxHeight: nil,
            maxBitrateBps: nil,
            compatibility: compatibility
        )
    }
}

struct LiveTvDeliveryOutput: Decodable, Sendable {
    let container: String
    let videoCodec: String
    let audioCodec: String
    let width: Int
    let height: Int
    let bitDepth: Int?
    let frameRate: LiveTvRational?
    let hdr: String?
    let audioChannels: Int
}

struct LiveTvDelivery: Decodable, Sendable {
    let output: LiveTvDeliveryOutput
    let videoAction: String
    let audioAction: String
    let packaging: String

    var playbackMethod: String {
        switch (videoAction, audioAction) {
        case ("copy", "copy"): return "Direct stream · no transcoding"
        case ("copy", "encode"): return "Audio transcoding · original video"
        case ("encode", "copy"): return "Video transcoding · original audio"
        case ("encode", "encode"): return "Video and audio transcoding"
        default: return "Playback method unavailable"
        }
    }

    var videoDescription: String {
        "\(Self.actionDescription(videoAction)) · \(output.videoCodec.uppercased()) · \(output.width)×\(output.height)"
    }

    var audioDescription: String {
        let channels = output.audioChannels == 1 ? "Mono"
            : output.audioChannels == 2 ? "Stereo" : "\(output.audioChannels) channels"
        return "\(Self.actionDescription(audioAction)) · \(output.audioCodec.uppercased()) · \(channels)"
    }

    private static func actionDescription(_ action: String) -> String {
        switch action {
        case "copy": return "Copied unchanged"
        case "encode": return "Transcoded"
        default: return "Unknown method"
        }
    }
}

struct LiveTvStarted: Decodable, Sendable {
    let sessionId: String
    let channel: LiveTvChannel
    let live: Bool
    var delivery: LiveTvDelivery? = nil
}

struct LiveTvStatus: Decodable, Sendable {
    let state: String
    let channel: LiveTvChannel?
    let ownerNodeId: String
    let encoder: String
    let outputHeight: Int
    let signal: LiveTvSignal?
    var delivery: LiveTvDelivery? = nil
}

struct LiveTvSignal: Decodable, Equatable, Sendable {
    let strengthPercent: Int?
    let qualityPercent: Int?
    let symbolQualityPercent: Int?
}

struct LiveTvSettings: Decodable, Equatable, Sendable {
    let libraryChannelsEnabled: Bool
    let liveTvEnabled: Bool
    let liveTvDeviceIpv4: String
    let liveTvOwnerNodeId: String
    let liveTvMaxSessions: Int
    let liveTvOutputHeight: Int
    let liveTvMaxOutputHeight: Int
    let liveTvConfigGeneration: Int64
    let liveTvTransitionFromOwnerNodeId: String
    let liveTvTransitionDrainBefore: Int64

    private enum CodingKeys: String, CodingKey {
        case libraryChannelsEnabled
        case liveTvEnabled
        case liveTvDeviceIpv4
        case liveTvOwnerNodeId
        case liveTvMaxSessions
        case liveTvOutputHeight
        case liveTvMaxOutputHeight
        case liveTvConfigGeneration
        case liveTvTransitionFromOwnerNodeId
        case liveTvTransitionDrainBefore
    }

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        // Older servers do not send this key. During a rolling upgrade the
        // safe representation is the old behaviour: authoring stays visible,
        // while resolve/session admission remains off until an administrator
        // explicitly enables it on a server that understands the switch.
        libraryChannelsEnabled = try values.decodeIfPresent(
            Bool.self,
            forKey: .libraryChannelsEnabled
        ) ?? false
        liveTvEnabled = try values.decode(Bool.self, forKey: .liveTvEnabled)
        liveTvDeviceIpv4 = try values.decode(String.self, forKey: .liveTvDeviceIpv4)
        liveTvOwnerNodeId = try values.decode(String.self, forKey: .liveTvOwnerNodeId)
        liveTvMaxSessions = try values.decode(Int.self, forKey: .liveTvMaxSessions)
        liveTvOutputHeight = try values.decode(Int.self, forKey: .liveTvOutputHeight)
        liveTvMaxOutputHeight = try values.decodeIfPresent(Int.self, forKey: .liveTvMaxOutputHeight) ?? 0
        liveTvConfigGeneration = try values.decode(Int64.self, forKey: .liveTvConfigGeneration)
        liveTvTransitionFromOwnerNodeId = try values.decode(
            String.self,
            forKey: .liveTvTransitionFromOwnerNodeId
        )
        liveTvTransitionDrainBefore = try values.decode(
            Int64.self,
            forKey: .liveTvTransitionDrainBefore
        )
    }
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
    case configure(ipv4: String, owner: String, sessions: Int, height: Int, maxHeight: Int = 0)
    case enabled(Bool)
    case libraryChannelsEnabled(Bool)
    case fencedOwner(owner: String, cutoff: Int64)

    func body(generation: Int64) throws -> Data {
        var fields: [String: Any] = ["live_tv_config_generation": generation]
        switch self {
        case let .configure(ipv4, owner, sessions, height, maxHeight):
            fields["live_tv_device_ipv4"] = ipv4
            fields["live_tv_owner_node_id"] = owner
            fields["live_tv_max_sessions"] = sessions
            fields["live_tv_output_height"] = height
            fields["live_tv_max_output_height"] = maxHeight
        case let .enabled(enabled): fields["live_tv_enabled"] = enabled
        case let .libraryChannelsEnabled(enabled): fields["library_channels_enabled"] = enabled
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
        case "source_format_changed": return "The broadcast changed format. plurx will select a fresh compatible route."
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
    private let compatibilityLock = NSLock()
    private var nextCompatibility: LiveTvCompatibility?

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

    func retryCompatibility(_ compatibility: LiveTvCompatibility) {
        compatibilityLock.lock()
        nextCompatibility = compatibility
        compatibilityLock.unlock()
    }

    private func takeCompatibility() -> LiveTvCompatibility? {
        compatibilityLock.lock()
        defer { compatibilityLock.unlock() }
        let value = nextCompatibility
        nextCompatibility = nil
        return value
    }

    func start(_ channel: String) async throws -> LiveTvStarted {
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        let body = try encoder.encode(["playback": LiveTvPlaybackEnvelope.current(
            compatibility: takeCompatibility()
        )])
        return try decode(LiveTvStarted.self, data: await request("live-tv/channels/\(Self.pathComponent(channel))/sessions",
                                                              method: "POST", authenticated: true, body: body, session: transport))
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

    /// The programme guide. A read of the owner's cache: it never triggers a
    /// fetch, so it is always fast and always answers — including with
    /// `freshness: "unavailable"`, which the client draws rather than retries.
    func guide(from: Int? = nil, hours: Int? = nil) async throws -> LiveTvGuide {
        var query: [String] = []
        if let from { query.append("from=\(from)") }
        if let hours { query.append("hours=\(max(1, min(72, hours)))") }
        let suffix = query.isEmpty ? "" : "?" + query.joined(separator: "&")
        return try decode(LiveTvGuide.self,
                          data: await request("live-tv/guide" + suffix, authenticated: true, session: transport))
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
struct LiveTvFileBarrierStore: LiveTvBarrierStore {
    private func url() throws -> URL {
#if os(tvOS)
        // tvOS does not guarantee an Application Support directory in an
        // app's local container. The restart fence lives for only 90 seconds,
        // contains no capability or user data, and must be writable before a
        // tuner request leaves the device, so keep it in the platform's
        // supported local cache area instead.
        return try FileManager.default.url(for: .cachesDirectory, in: .userDomainMask,
                                           appropriateFor: nil, create: true)
            .appendingPathComponent("live-tv-start.pending")
#else
        try FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask,
                                    appropriateFor: nil, create: true).appendingPathComponent("live-tv-start.pending")
#endif
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
