import Foundation

struct CinemaRemoteControl: Codable, Equatable {
    let controlEpoch: UUID
    let activeGrantID: UUID
    let controllerName: String
    enum CodingKeys: String, CodingKey { case controlEpoch = "control_epoch", activeGrantID = "active_grant_id", controllerName = "controller_name" }
}
struct CinemaRemoteDevice: Codable, Identifiable {
    let receiverID: UUID
    let name: String
    let platform: String
    let target: CinemaRemoteTarget?
    let available: Bool
    let busy: Bool
    let paired: Bool
    var id: UUID { receiverID }
    enum CodingKeys: String, CodingKey { case receiverID = "receiver_id", name, platform, target, available, busy, paired }
}
struct CinemaRemotePendingPairing: Codable, Identifiable {
    let pendingID: UUID
    let controllerName: String
    var id: UUID { pendingID }
    enum CodingKeys: String, CodingKey { case pendingID = "pending_id", controllerName = "controller_name" }
}
struct CinemaRemoteState: Codable {
    let stateRevision: UInt64
    let contextRevision: UInt64
    let focusRevision: UInt64
    let route: String
    let capabilities: [CinemaRemoteAction.Kind]
    let focusedLabel: String?
    let credits: [CinemaRemoteCredit]
    let textNonce: UUID?
    let playback: CinemaRemotePlaybackSummary?
    enum CodingKeys: String, CodingKey {
        case stateRevision = "state_revision", contextRevision = "context_revision", focusRevision = "focus_revision"
        case route, capabilities, focusedLabel = "focused_label", credits, textNonce = "text_nonce", playback
    }
}
struct CinemaRemoteRegistration: Decodable {
    let receiverID: UUID
    let receiverSecret: String
    enum CodingKeys: String, CodingKey { case receiverID = "receiver_id", receiverSecret = "receiver_secret" }
}
struct CinemaRemoteTargetResponse: Decodable { let target: CinemaRemoteTarget }
struct CinemaRemoteDevicesResponse: Decodable {
    let receivers: [CinemaRemoteDevice]
    let unavailableNodes: [String]
    enum CodingKeys: String, CodingKey { case receivers, unavailableNodes = "unavailable_nodes" }
}
struct CinemaRemotePollResponse: Decodable {
    let target: CinemaRemoteTarget
    let responseRevision: UInt64
    let deliveryID: UInt64
    let control: CinemaRemoteControl?
    let commands: [CinemaRemoteCommand]
    let pairings: [CinemaRemotePendingPairing]
    enum CodingKeys: String, CodingKey { case target, responseRevision = "response_revision", deliveryID = "delivery_id", control, commands, pairings }
}
struct CinemaRemoteStateResponse: Decodable {
    let target: CinemaRemoteTarget
    let responseRevision: UInt64
    let control: CinemaRemoteControl?
    let state: CinemaRemoteState?
    let outcomes: [RemoteReceiverGuard.Acknowledgement]
    enum CodingKeys: String, CodingKey { case target, responseRevision = "response_revision", control, state, outcomes }
}
struct CinemaRemoteControlResponse: Decodable {
    let target: CinemaRemoteTarget
    let responseRevision: UInt64
    let control: CinemaRemoteControl?
    enum CodingKeys: String, CodingKey { case target, responseRevision = "response_revision", control }
}
struct CinemaRemoteChallenge: Decodable {
    let target: CinemaRemoteTarget
    let challengeID: UUID
    let code: String
    let expiresInMs: UInt64
    enum CodingKeys: String, CodingKey { case target, challengeID = "challenge_id", code, expiresInMs = "expires_in_ms" }
}
struct CinemaRemotePairClaim: Decodable {
    let pendingID: UUID
    let pollSecret: String
    enum CodingKeys: String, CodingKey { case pendingID = "pending_id", pollSecret = "poll_secret" }
}
struct CinemaRemotePairResult: Decodable {
    let status: String
    let grantID: UUID?
    let grantSecret: String?
    let receiverID: UUID?
    enum CodingKeys: String, CodingKey { case status, grantID = "grant_id", grantSecret = "grant_secret", receiverID = "receiver_id" }
}
struct CinemaRemoteAccepted: Decodable { let accepted: Bool }
struct CinemaRemoteQueued: Decodable {
    let queued: Bool
    let controlEpoch: UUID
    let sequence: UInt64
    enum CodingKeys: String, CodingKey { case queued, controlEpoch = "control_epoch", sequence }
}

struct CinemaRemoteGrantInfo: Decodable, Identifiable {
    let id: UUID
    let receiverID: UUID
    let name: String
    let createdAt: UInt64
    enum CodingKeys: String, CodingKey { case id, receiverID = "receiver_id", name, createdAt = "created_at" }
}
struct CinemaRemoteGrants: Decodable { let grants: [CinemaRemoteGrantInfo] }
struct CinemaRemoteRevocation: Decodable { let revoked: Bool }

struct CinemaRemoteAPI {
    enum Proof { case receiver(String), grant(String), pairing(String) }
    let origin: String
    let token: String
    let generation: UInt64
    var transport = RemoteHTTPTransport.session
    var isCurrent: Bool {
        let current = Session.shared.playbackAuthorization
        return current.generation == generation && Session.canonicalOrigin(current.origin) == origin && current.token == token
    }
    private func request<T: Decodable>(_ path: String, method: String = "POST", body: [String: Any]? = nil,
                                       proof: Proof? = nil, maximumBytes: Int = 64 * 1_024) async throws -> T {
        guard isCurrent, Session.canonicalOrigin(origin) == origin,
              let url = URL(string: origin + "/api/remote/v1" + path) else { throw CinemaRemoteOutcome.unavailable }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.timeoutInterval = 25
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.setValue("Bearer " + token, forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        if let proof {
            let header: String
            let secret: String
            switch proof {
            case .receiver(let value): header = "X-Cinema-Receiver-Secret"; secret = value
            case .grant(let value): header = "X-Cinema-Grant-Secret"; secret = value
            case .pairing(let value): header = "X-Cinema-Pairing-Secret"; secret = value
            }
            guard RemoteSecretStorage.validSecret(secret) else { throw CinemaRemoteOutcome.unauthorized }
            request.setValue(secret, forHTTPHeaderField: header)
        }
        if var body {
            body["version"] = CinemaRemoteCommand.versionName
            let data = try JSONSerialization.data(withJSONObject: body)
            let bound = path == "/presence" ? 64 * 1_024 : CinemaRemoteCommand.maximumBytes
            guard data.count <= bound else { throw CinemaRemoteOutcome.invalid }
            request.httpBody = data
        }
        let (bytes, response) = try await transport.bytes(for: request)
        guard let http = response as? HTTPURLResponse else { throw CinemaRemoteOutcome.unavailable }
        var data = Data()
        for try await byte in bytes {
            guard data.count < maximumBytes else { throw CinemaRemoteOutcome.invalid }
            data.append(byte)
        }
        guard isCurrent, !Task.isCancelled else { throw CancellationError() }
        guard (200..<300).contains(http.statusCode) else {
            switch http.statusCode {
            case 401, 403: throw CinemaRemoteOutcome.unauthorized
            case 409, 429: throw CinemaRemoteOutcome.busy
            case 400: throw CinemaRemoteOutcome.invalid
            default: throw CinemaRemoteOutcome.unavailable
            }
        }
        return try Self.decodeResponse(data)
    }
    static func decodeResponse<T: Decodable>(_ data: Data) throws -> T {
        guard data.count <= 64 * 1_024 else { throw CinemaRemoteOutcome.invalid }
        try RemoteStrictJSON.check(data)
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              object["version"] as? String == CinemaRemoteCommand.versionName else { throw CinemaRemoteOutcome.invalid }
        if let commands = object["commands"] as? [[String: Any]] {
            guard commands.count <= 32 else { throw CinemaRemoteOutcome.invalid }
            for command in commands { _ = try CinemaRemoteCommand.decode(JSONSerialization.data(withJSONObject: command)) }
        }
        return try JSONDecoder().decode(T.self, from: data)
    }
    static func object<T: Encodable>(_ value: T) throws -> [String: Any] {
        guard let object = try JSONSerialization.jsonObject(with: JSONEncoder().encode(value)) as? [String: Any] else { throw CinemaRemoteOutcome.invalid }
        return object
    }
    func register(name: String) async throws -> CinemaRemoteRegistration {
        try await request("/receivers", body: ["name": RemoteTextBounds.label(name, maximumBytes: 80), "platform": "apple_tv"])
    }
    func devices() async throws -> CinemaRemoteDevicesResponse { try await request("/receivers", method: "GET") }
    func session(receiverID: UUID, foregroundID: UUID, secret: String) async throws -> CinemaRemoteTargetResponse {
        try await request("/sessions", body: ["receiver_id": receiverID.uuidString, "foreground_id": foregroundID.uuidString], proof: .receiver(secret))
    }
    func presence(target: CinemaRemoteTarget, state: CinemaRemoteState, secret: String) async throws -> CinemaRemoteAccepted {
        try await request("/presence", body: ["target": Self.object(target), "state": Self.object(state)], proof: .receiver(secret))
    }
    func poll(target: CinemaRemoteTarget, after: UInt64, responseRevision: UInt64, secret: String) async throws -> CinemaRemotePollResponse {
        try await request("/poll", body: ["target": Self.object(target), "after_delivery_id": after, "after_response_revision": responseRevision, "wait_ms": 20_000], proof: .receiver(secret))
    }
    func ack(target: CinemaRemoteTarget, outcomes: [RemoteReceiverGuard.Acknowledgement], secret: String) async throws -> CinemaRemoteAccepted {
        let value = try JSONSerialization.jsonObject(with: JSONEncoder().encode(outcomes))
        return try await request("/ack", body: ["target": Self.object(target), "outcomes": value], proof: .receiver(secret))
    }
    func state(target: CinemaRemoteTarget, grantID: UUID, after: UInt64, secret: String) async throws -> CinemaRemoteStateResponse {
        try await request("/state", body: ["target": Self.object(target), "grant_id": grantID.uuidString, "after_revision": after, "wait_ms": 20_000], proof: .grant(secret))
    }
    func control(target: CinemaRemoteTarget, grantID: UUID, action: String, epoch: UUID?, secret: String) async throws -> CinemaRemoteControlResponse {
        try await request("/control", body: ["target": Self.object(target), "grant_id": grantID.uuidString, "action": action,
                                            "control_epoch": epoch?.uuidString as Any? ?? NSNull()], proof: .grant(secret))
    }
    func send(_ command: CinemaRemoteCommand, secret: String) async throws -> CinemaRemoteQueued {
        try command.validate()
        return try await request("/commands", body: Self.object(command), proof: .grant(secret))
    }
    func grants() async throws -> CinemaRemoteGrants { try await request("/grants", method: "GET") }
    func revokeGrant(_ id: UUID) async throws -> CinemaRemoteRevocation { try await request("/grants/" + id.uuidString, method: "DELETE") }
    func unregister(_ id: UUID) async throws -> CinemaRemoteRevocation { try await request("/receivers/" + id.uuidString, method: "DELETE") }
    func pairingStart(target: CinemaRemoteTarget, secret: String) async throws -> CinemaRemoteChallenge {
        try await request("/pairing/start", body: ["target": Self.object(target)], proof: .receiver(secret))
    }
    func pairingClaim(target: CinemaRemoteTarget, challengeID: UUID?, code: String, name: String) async throws -> CinemaRemotePairClaim {
        try await request("/pairing/claim", body: ["target": Self.object(target), "challenge_id": challengeID.map { $0.uuidString as Any } ?? NSNull(),
                                                 "code": code, "controller_name": RemoteTextBounds.label(name, maximumBytes: 80)])
    }
    func pairingResult(target: CinemaRemoteTarget, pendingID: UUID, secret: String) async throws -> CinemaRemotePairResult {
        try await request("/pairing/result", body: ["target": Self.object(target), "pending_id": pendingID.uuidString], proof: .pairing(secret))
    }
    func pairingApprove(target: CinemaRemoteTarget, pendingID: UUID, approve: Bool, secret: String) async throws -> CinemaRemoteAccepted {
        try await request("/pairing/approve", body: ["target": Self.object(target), "pending_id": pendingID.uuidString, "approve": approve], proof: .receiver(secret))
    }
}

private final class RemoteRedirectBlocker: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}
enum RemoteHTTPTransport {
    private static let redirectBlocker = RemoteRedirectBlocker()
    static let session: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.urlCache = nil
        configuration.httpShouldSetCookies = false
        configuration.timeoutIntervalForRequest = 25
        configuration.timeoutIntervalForResource = 30
        return URLSession(configuration: configuration, delegate: redirectBlocker, delegateQueue: nil)
    }()
}
