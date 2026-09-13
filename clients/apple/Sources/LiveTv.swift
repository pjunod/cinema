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
    /// The start protocols this ingress negotiated with the tuner owner — the
    /// intersection of what its own public surface accepts and what the owner
    /// supports. Protocol `3` is client request ids plus the
    /// `/api/v1/live-tv/starts/*` recovery routes. Absent from an ingress
    /// older than the contract, which means legacy behaviour: no
    /// `request_id` on the start body and no recovery routes.
    let protocols: [Int]?
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

/// The guide's own advisory card, a second `checks` array beside the session
/// one. Every row says what would have to be true for the guide to work and
/// whether it is true right now; the server marks the whole document
/// `advisory`, and nothing here refuses a save or a toggle.
///
/// Decoded loosely on purpose: a server that adds a row, or a field this view
/// does not draw, must not turn the card into an error. Only `checks` is
/// required, and even that is answered as empty rather than thrown away.
struct LiveTvGuideReadiness: Decodable, Sendable {
    let advisory: Bool
    let source: String
    let freshness: String
    let ageSeconds: Int
    let programmes: Int
    let matchedChannels: Int
    let lineupChannels: Int
    let refreshIntervalSeconds: Int
    let checks: [LiveTvReadiness.Check]

    /// Advisory, so this is a summary line, never a gate.
    var allMet: Bool { checks.allSatisfy(\.ready) }

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        func read<T: Decodable>(_ key: CodingKeys, _ fallback: T) -> T {
            ((try? values.decodeIfPresent(T.self, forKey: key)) ?? nil) ?? fallback
        }
        advisory = read(.advisory, true)
        source = read(.source, "unknown")
        freshness = read(.freshness, "unavailable")
        ageSeconds = read(.ageSeconds, 0)
        programmes = read(.programmes, 0)
        matchedChannels = read(.matchedChannels, 0)
        lineupChannels = read(.lineupChannels, 0)
        refreshIntervalSeconds = read(.refreshIntervalSeconds, 0)
        checks = read(.checks, [LiveTvReadiness.Check]())
    }

    private enum CodingKeys: String, CodingKey {
        case advisory, source, freshness, ageSeconds, programmes
        case matchedChannels, lineupChannels, refreshIntervalSeconds, checks
    }
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
    /// The typed envelope's two extra flat fields, when the server sent them
    /// (docs/features/LIVE-TV-RELIABILITY-IMPLEMENTATION.md §3.11). `retry` is
    /// `now` | `later` | `never`. `ownerDecided` is `false` unless the body
    /// said `true` — the safe default for every code the table does not list,
    /// because "the owner never saw this" is what makes a hint worth keeping.
    var retry: String?
    var ownerDecided = false
    /// The HTTP status the body arrived with; `nil` when nothing arrived.
    var status: Int?

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
        // The one answer that is not an answer. It never refuses the next
        // press: whatever the server may or may not have started is held by
        // the persisted hint, and the press after this one retires it.
        case "no_answer": return "The server did not answer. Press the channel again."
        case "invalid_request": return "This device sent a live-TV request the server could not read. Update the app."
        case "invalid_settings": return "Live TV settings are incomplete. An administrator can finish them in Settings → Developer."
        case "admin_required": return "Only an administrator can change Live TV settings."
        default: return "The tuner owner is unavailable. Check the server and its network connection."
        }
    }
}

/// What the owner said about a request id the client still holds a hint for.
/// `session` is present exactly when `outcome` is `live`, and is shaped like a
/// successful start so a resume enters playback through the same path.
struct LiveTvResumeAnswer: Decodable, Sendable {
    let outcome: String
    let session: LiveTvStarted?

    /// Spelled out because this type is `Decodable` only and writes its own
    /// `init(from:)`, so nothing else would synthesise them. Both names are
    /// single words, which is what `.convertFromSnakeCase` leaves them as.
    private enum CodingKeys: String, CodingKey {
        case outcome, session
    }

    init(outcome: String, session: LiveTvStarted? = nil) {
        self.outcome = outcome
        self.session = session
    }

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        outcome = try values.decode(String.self, forKey: .outcome)
        // A `live` answer whose session cannot be read is not a reattach: the
        // reducer's `reattach` still fires, the lease finds no session, and
        // the hint is kept for the next press. Never a decode failure.
        session = (try? values.decodeIfPresent(LiveTvStarted.self, forKey: .session)) ?? nil
    }
}

protocol LiveTvRequests {
    /// `requestId` is `nil` exactly when the ingress did not negotiate
    /// protocol 3: an older one rejects the field outright.
    func start(_ channel: String, requestId: String?) async throws -> LiveTvStarted
    func release(_ capability: String) async throws
    /// `POST /api/v1/live-tv/starts/{id}/resume`.
    func resume(_ requestId: String) async throws -> LiveTvResumeAnswer
    /// `DELETE /api/v1/live-tv/starts/{id}` — fire and forget.
    func retire(_ requestId: String) async throws
    /// Whether the LAST channels response listed protocol `3`. The only
    /// signal for sending a `request_id` and for using the recovery routes.
    ///
    /// A function rather than a property, and `async`, so that an actor- or
    /// `@MainActor`-isolated implementation can witness it — a synchronous
    /// requirement cannot be satisfied by isolated state.
    func recoveryRoutesAvailable() async -> Bool
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
    private let protocolLock = NSLock()
    private var negotiatedProtocols: [Int]?

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
            struct WireFailure: Decodable {
                let code: String
                let retry: String?
                let ownerDecided: Bool?
                enum CodingKeys: String, CodingKey {
                    case code, retry
                    case ownerDecided = "owner_decided"
                }
            }
            if let failure = try? JSONDecoder().decode(WireFailure.self, from: data) {
                throw LiveTvFailure(code: failure.code, retry: failure.retry,
                                    ownerDecided: failure.ownerDecided ?? false,
                                    status: response.statusCode)
            }
            if method == "DELETE", response.statusCode == 404 || response.statusCode == 410 { return Data() }
            if response.statusCode == 401 || response.statusCode == 403 {
                throw LiveTvFailure(code: "admin_required", status: response.statusCode)
            }
            // An untyped intermediary failure on a start POST is not an
            // answer: nobody typed a verdict, so the start may or may not
            // exist. `no_answer` is what the reducer replays and keeps a hint
            // for — never a refusal to press again.
            if method == "POST", path.hasSuffix("/sessions") {
                throw LiveTvFailure(code: "no_answer", status: response.statusCode)
            }
            throw LiveTvFailure(code: "owner_unavailable", status: response.statusCode)
        }
        return data
    }

    private func decode<T: Decodable>(_ type: T.Type, data: Data) throws -> T {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(type, from: data)
    }

    func lineup() async throws -> LiveTvLineup {
        let lineup = try decode(LiveTvLineup.self,
                                data: await request("live-tv/channels", authenticated: true, session: transport))
        // Negotiation is per lineup read, not per process: an ingress can be
        // replaced under a running app, in either direction.
        protocolLock.lock()
        negotiatedProtocols = lineup.protocols
        protocolLock.unlock()
        return lineup
    }

    func recoveryRoutesAvailable() async -> Bool {
        protocolLock.lock()
        defer { protocolLock.unlock() }
        return LiveTvStartReducer.negotiatesRecovery(protocols: negotiatedProtocols)
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

    /// The public start body. `request_id` is omitted entirely when protocol 3
    /// was not negotiated — an older ingress parses this body with
    /// `deny_unknown_fields` and would answer 400 to the field's mere
    /// presence. A `nil` Optional property is omitted by the synthesised
    /// encoder, which is exactly that.
    private struct StartBody: Encodable {
        let playback: LiveTvPlaybackEnvelope
        let requestId: String?
    }

    func start(_ channel: String, requestId: String?) async throws -> LiveTvStarted {
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        let body = try encoder.encode(StartBody(
            playback: LiveTvPlaybackEnvelope.current(compatibility: takeCompatibility()),
            requestId: requestId
        ))
        return try decode(LiveTvStarted.self, data: await request("live-tv/channels/\(Self.pathComponent(channel))/sessions",
                                                              method: "POST", authenticated: true, body: body, session: transport))
    }

    func resume(_ requestId: String) async throws -> LiveTvResumeAnswer {
        try decode(LiveTvResumeAnswer.self,
                   data: await request("live-tv/starts/\(Self.pathComponent(requestId))/resume",
                                       method: "POST", authenticated: true, session: control))
    }

    /// Retiring is a fence, not a question: a 404 or 410 means there was
    /// nothing left to retire, which `request` already answers as success.
    func retire(_ requestId: String) async throws {
        _ = try await request("live-tv/starts/\(Self.pathComponent(requestId))",
                              method: "DELETE", authenticated: true, session: cleanup)
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

    /// The guide's advisory card. A read of what the owner already knows: it
    /// never triggers a refresh and never gates anything the operator can do.
    func guideReadiness() async throws -> LiveTvGuideReadiness {
        try decode(LiveTvGuideReadiness.self,
                   data: await request("live-tv/guide/readiness", authenticated: true, session: transport))
    }
}

private final class LiveTvNoRedirects: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}

/// The one thing Live TV persists, and the whole of it: which start this
/// device is holding, and when it last proved something was watching it.
///
/// Never a capability, never a token, never a channel id or a tuner URL. The
/// request id is enough to retire a start or to resume it; `resume` hands the
/// capability back over the authenticated channel and it stays in memory, as
/// it always has.
struct LiveTvStartHint: Codable, Equatable, Sendable {
    let requestId: String
    /// Unix milliseconds.
    var touchedAt: Int64
}

protocol LiveTvHintStore {
    /// `nil` means "no hint" — including when the file is simply not there.
    func read() -> LiveTvStartHint?
    func write(_ hint: LiveTvStartHint) throws
    func clear()
}

/// One small JSON document, written atomically, at the path the 90-second
/// restart barrier's marker used to occupy.
///
/// A missing file reads as "no hint" and NEVER as an error. That matters most
/// on tvOS, where this directory is Caches and the system may purge it at any
/// moment: a purged hint costs a resume — the viewer sees the channel list and
/// presses once — and must never cost a refusal to start. Every failure below
/// is swallowed for the same reason (guardrail §4.4: no client-side refusal to
/// start, ever again).
struct LiveTvStartHintStore: LiveTvHintStore {
    /// The file the 90-second restart barrier wrote. Deleting it on the first
    /// construction of a hint store is the whole of the migration; nothing
    /// ever reads it again.
    static let legacyBarrierMarker = "live-tv-start.pending"
    static let hintFile = "live-tv-start.hint"

    init() { try? FileManager.default.removeItem(at: Self.url(Self.legacyBarrierMarker)) }

    private static func url(_ name: String) -> URL {
#if os(tvOS)
        // tvOS does not guarantee an Application Support directory in an app's
        // local container, so the hint lives in the platform's supported local
        // cache area. tvOS may purge it; `read()` answers `nil` and the viewer
        // gets the channel list.
        let directory = try? FileManager.default.url(for: .cachesDirectory, in: .userDomainMask,
                                                     appropriateFor: nil, create: true)
#else
        let directory = try? FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask,
                                                     appropriateFor: nil, create: true)
#endif
        return (directory ?? URL(fileURLWithPath: NSTemporaryDirectory()))
            .appendingPathComponent(name)
    }

    func read() -> LiveTvStartHint? {
        guard let data = try? Data(contentsOf: Self.url(Self.hintFile)) else { return nil }
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        guard let hint = try? decoder.decode(LiveTvStartHint.self, from: data),
              LiveTvStartReducer.isRequestId(hint.requestId) else { return nil }
        return hint
    }

    func write(_ hint: LiveTvStartHint) throws {
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        try encoder.encode(hint).write(to: Self.url(Self.hintFile), options: .atomic)
    }

    func clear() { try? FileManager.default.removeItem(at: Self.url(Self.hintFile)) }
}

/// What the server said about a start, reduced to the four facts the shared
/// fixture pins. Built from a thrown `LiveTvFailure`, or by a test from
/// `tests/playback/live-tv-start-cases.json`.
struct LiveTvStartAnswer: Equatable, Sendable {
    /// The HTTP status the body arrived with; `nil` when nothing arrived.
    let status: Int?
    /// The `code` of a typed error body; `nil` when nothing typed a verdict.
    let code: String?
    let retry: String?
    let ownerDecided: Bool

    init(status: Int? = nil, code: String? = nil, retry: String? = nil, ownerDecided: Bool = false) {
        self.status = status
        self.code = code
        self.retry = retry
        self.ownerDecided = ownerDecided
    }

    /// A `URLSession` error — a timeout, a dropped connection — is not an
    /// answer at all, and neither is an untyped intermediary failure: both
    /// leave `code` nil and the reducer says `no_answer`.
    init(error: Error) {
        let failure = error as? LiveTvFailure
        status = failure?.status
        // `no_answer` is this client's own word for "nothing typed a verdict",
        // so it is not a code the server sent and must not read as one.
        if let failure, failure.code != "no_answer" { code = failure.code } else { code = nil }
        retry = failure?.retry
        ownerDecided = failure?.ownerDecided ?? false
    }
}

/// What the client does with one start answer.
struct LiveTvStartVerdict: Equatable, Sendable {
    /// The copy key the viewer is shown — a `LiveTvFailure` code.
    let render: String
    /// Whether the error view offers a retry control.
    let offerRetry: Bool
    /// Whether the persisted hint survives this answer.
    let keepHint: Bool
    /// Whether the lease re-sends the POST once with the same request id.
    let replay: Bool
}

/// The reducer `tests/playback/live-tv-start-cases.json` prescribes, shared
/// word for word with the web and Android leases. A pure function: it reads no
/// clock, touches no disk, and has no notion of refusing to start.
enum LiveTvStartReducer {
    /// A typed 4xx the ingress decided before the request ever reached an
    /// owner. There is no start behind these, so there is nothing to keep a
    /// hint for and nothing a retry would fix.
    static let ingressRefusals: Set<String> = [
        "invalid_request", "admin_required", "invalid_settings", "live_tv_disabled",
    ]

    /// Every code the client draws in its own words. Anything else — a typed
    /// refusal minted outside the Live TV module, or one from a newer server —
    /// is drawn as `owner_unavailable`, which is what the fixture's
    /// `node_maintenance` row says.
    static let rendered: Set<String> = [
        "live_tv_disabled", "live_tv_protocol_unready", "tuner_capacity",
        "tuner_unavailable", "channel_not_found", "drm_unsupported",
        "codec_unsupported", "startup_timeout", "source_format_changed",
        "stream_failed", "capability_expired", "settings_conflict",
        "invalid_request", "invalid_settings", "admin_required",
        "owner_unavailable", "no_answer",
    ]

    /// The answer that never arrived. The hint is the only handle on a start
    /// that may or may not exist, so it is kept and the POST is replayed once.
    static let noAnswer = LiveTvStartVerdict(render: "no_answer", offerRetry: true,
                                             keepHint: true, replay: true)

    static func verdict(_ answer: LiveTvStartAnswer) -> LiveTvStartVerdict {
        guard let code = answer.code else { return noAnswer }
        // A typed 4xx naming one of the ingress's own refusals is the only
        // thing besides `owner_decided: true` that proves no owner ever saw
        // this request. Everything else — including every code this client has
        // never heard of — keeps the hint.
        let ingressRefused = ingressRefusals.contains(code)
            && (400..<500).contains(answer.status ?? 0)
        let decided = answer.ownerDecided || ingressRefused
        return LiveTvStartVerdict(
            render: rendered.contains(code) ? code : "owner_unavailable",
            offerRetry: answer.retry != "never" && !ingressRefused,
            keepHint: !decided,
            replay: false
        )
    }

    /// What `resumeIfRecent()` does with a resume answer, as the fixture's
    /// `then` column names it. `nil` is a resume that did not answer.
    static func resumeAction(outcome: String?) -> LiveTvResumeAction {
        switch outcome {
        case .some("live"): .reattach
        case .some("ended"), .some("retired"): .clearHintWait
        // `pending` — an activation is still in flight from an ingress — and
        // anything that did not answer both keep the hint and show the list.
        default: .keepHintWait
        }
    }

    /// Protocol `3` is client request ids plus the `/live-tv/starts/*`
    /// recovery routes, and the last channels response is the only signal for
    /// it. No `protocols` field at all means an ingress older than the
    /// contract: legacy behaviour, and any hint already on disk is left where
    /// it is for a server that later does negotiate 3.
    static func negotiatesRecovery(protocols: [Int]?) -> Bool {
        protocols?.contains(3) ?? false
    }

    /// 32 lower-case hex characters — the shape the ingress validates with
    /// `^[0-9a-f]{32}$`.
    static func isRequestId(_ value: String) -> Bool {
        value.count == 32 && value.allSatisfy { "0123456789abcdef".contains($0) }
    }

    /// 128 bits of it. A handle, not a secret: replaying it joins the same
    /// owner session, and it is only ever sent over the authenticated channel.
    static func newRequestId() -> String {
        (0..<16).map { _ in String(format: "%02x", UInt8.random(in: 0...255)) }.joined()
    }
}

enum LiveTvResumeAction: String, Equatable, Sendable {
    case reattach
    case clearHintWait = "clear_hint_wait"
    case keepHintWait = "keep_hint_wait"
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
    private let hints: LiveTvHintStore
    private var tail: Task<Void, Never>?
    private var generation = 0
    private(set) var current: LiveTvStarted?
    /// The request id `current` was started (or resumed) with, so a confirmed
    /// release knows which hint it may forget. `nil` against a legacy ingress,
    /// which negotiates no request ids at all.
    private(set) var held: String?
    /// The last time the hint's `touched_at` was rewritten. The heartbeat
    /// beats every 5 s; rewriting the file that often buys nothing on a client
    /// with no sibling documents to inform, and the Android review's ANR
    /// finding about per-heartbeat writes applies here just as well.
    private var lastTouch: ContinuousClock.Instant?
    private static let touchInterval = Duration.seconds(30)

    init(requests: LiveTvRequests, hints: LiveTvHintStore? = nil) {
        self.requests = requests
        // The hint is device-wide and carries no account credentials, so a
        // profile or server switch neither reads nor invalidates it: the id is
        // meaningless to a server that did not issue it, and the owner that
        // did reaps its session at 45 s idle.
        self.hints = hints ?? LiveTvStartHintStore()
    }

    // MARK: - the press

    /// The press flow of §3.16. It can fail, and it can be superseded, but it
    /// can never refuse: there is no state in this lease that says "not yet".
    func start(_ channel: String) async throws -> LiveTvStarted? {
        generation += 1
        let expected = generation, previous = tail
        let operation = Task { @MainActor in
            await previous?.value
            try await releaseCurrent()
            guard generation == expected else { return nil as LiveTvStarted? }

            let recovery = await requests.recoveryRoutesAvailable()
            // Whatever a killed process left behind is retired in the
            // background. The press never waits on it, and a retire that fails
            // simply leaves the hint for the next press or the owner's 45 s
            // reap — a hint that cannot be retired is kept, never a refusal.
            if recovery { retireOrphanedHint() }
            guard generation == expected else { return nil }

            let requestId = recovery ? LiveTvStartReducer.newRequestId() : nil
            // Persisted before the POST leaves the device, including a process
            // killed mid-POST: the id is the only handle on a start the server
            // may already have made.
            if let requestId { remember(requestId) }
            let info = try await dispatch(channel, requestId: requestId)
            guard !info.sessionId.isEmpty, info.sessionId.utf8.count <= 1024 else {
                // A 2xx nobody can use. The start may well exist, so the hint
                // stays and the next press retires it.
                throw LiveTvFailure(code: "no_answer")
            }
            current = info
            held = requestId
            guard info.live, generation == expected else {
                try await releaseCurrent()
                return nil
            }
            return info
        }
        tail = Task { _ = try? await operation.value }
        return try await operation.value
    }

    /// The POST, and the one replay the contract allows. Every outcome runs
    /// through the shared reducer, so what is rendered and what survives on
    /// disk are the fixture's answers rather than this file's opinion.
    private func dispatch(_ channel: String, requestId: String?) async throws -> LiveTvStarted {
        var verdict = LiveTvStartReducer.noAnswer
        for attempt in 0...LiveTvInputRouting.startReplayAttempts {
            do { return try await requests.start(channel, requestId: requestId) }
            catch {
                verdict = LiveTvStartReducer.verdict(LiveTvStartAnswer(error: error))
                // The replay carries the SAME request id: the owner joins it
                // to the session the first POST may already have created.
                if !verdict.replay || attempt == LiveTvInputRouting.startReplayAttempts { break }
            }
        }
        if !verdict.keepHint, let requestId { forget(requestId) }
        throw LiveTvFailure(code: verdict.render)
    }

    // MARK: - opening

    /// The open-time step of §3.16. Answers the session to attach when the
    /// owner still has it, and `nil` for everything else — including a resume
    /// that never answered, which keeps the hint for the next press.
    func resumeIfRecent() async -> LiveTvStarted? {
        guard current == nil, let hint = hints.read(),
              await requests.recoveryRoutesAvailable() else { return nil }
        generation += 1
        let expected = generation, previous = tail
        let operation = Task { @MainActor in
            await previous?.value
            guard generation == expected, current == nil else { return nil as LiveTvStarted? }
            let answer = try? await requests.resume(hint.requestId)
            switch LiveTvStartReducer.resumeAction(outcome: answer?.outcome) {
            case .reattach:
                guard let session = answer?.session, session.live,
                      !session.sessionId.isEmpty, session.sessionId.utf8.count <= 1024
                else { return nil }
                // A press that superseded this resume already released
                // whatever it found and is starting its own session. Leave the
                // hint: the next press retires it, and the owner reaps at 45 s.
                guard generation == expected else { return nil }
                current = session
                held = hint.requestId
                touchHint(force: true)
                return session
            case .clearHintWait:
                forget(hint.requestId)
                return nil
            case .keepHintWait:
                return nil
            }
        }
        tail = Task { _ = await operation.value }
        return await operation.value
    }

    // MARK: - release

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
        let requestId = held
        // A failed release keeps both the capability and the hint: the session
        // is still out there and the next press must still be able to name it.
        try await requests.release(info.sessionId)
        current = nil
        held = nil
        lastTouch = nil
        // A confirmed DELETE is a confirmed release — nothing is left to
        // resume, so a clean background shows the channel list.
        if let requestId { forget(requestId) }
    }

    // MARK: - the hint

    /// The heartbeat's other half. Rate-limited: `touched_at` informs nothing
    /// on this platform, and a file write every 5 s for the life of a session
    /// is exactly the cost the Android review refused.
    func touchHint(force: Bool = false) {
        guard let requestId = held else { return }
        let now = ContinuousClock.now
        if !force, let last = lastTouch, last.duration(to: now) < Self.touchInterval { return }
        lastTouch = now
        remember(requestId)
    }

    private func remember(_ requestId: String) {
        // A hint that cannot be written costs a resume, never a start: the
        // press proceeds either way (guardrail §4.4).
        try? hints.write(LiveTvStartHint(
            requestId: requestId,
            touchedAt: Int64(Date().timeIntervalSince1970 * 1000)
        ))
    }

    /// Forget the hint only if the file still names this start. A press that
    /// has already persisted its own hint must not have it deleted by a retire
    /// or a release that belongs to the start before it.
    private func forget(_ requestId: String) {
        guard hints.read()?.requestId == requestId else { return }
        hints.clear()
        if held == requestId { lastTouch = nil }
    }

    /// A hint on disk that this lease does not hold is an orphan, full stop.
    ///
    /// The contract's liveness probe (`retire_liveness_probe_ms`,
    /// `retire_orphan_after_keepalives`) exists because several web documents
    /// share one origin's storage and one of them may still be watching. One
    /// app process owns this file outright, and `releaseCurrent` forgets the
    /// hint of anything it was holding, so there is nothing here to probe —
    /// and a hint left seconds ago by a killed process is exactly the case
    /// that must be retired rather than left to the owner's 45 s reap.
    private func retireOrphanedHint() {
        guard let hint = hints.read(), hint.requestId != held else { return }
        Task { @MainActor [weak self] in
            guard let requests = self?.requests else { return }
            // Fire and forget. A typed 2xx (or the 404/410 that means there
            // was nothing there) forgets the hint; anything else leaves it.
            do { try await requests.retire(hint.requestId) } catch { return }
            self?.forget(hint.requestId)
        }
    }
}
