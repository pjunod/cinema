import Foundation

struct HomeInvitationAPI {
    let origin: String
    let token: String
    let authorizationGeneration: UInt64
    var transport = InvitationHTTPTransport.session
    var isCurrent: Bool {
        let auth = Session.shared.playbackAuthorization
        return auth.generation == authorizationGeneration && Session.canonicalOrigin(auth.origin) == origin && auth.token == token
    }
    private func request(_ path: String, body: [String: Any]?, phoneSecret: String? = nil,
                         grantSecret: String? = nil, method: String = "POST") async throws -> Data {
        guard isCurrent, Session.canonicalOrigin(origin) == origin,
              let url = URL(string: origin + "/api/remote/v1" + path) else { throw CancellationError() }
        var request = URLRequest(url: url)
        request.httpMethod = method; request.timeoutInterval = 25; request.cachePolicy = .reloadIgnoringLocalCacheData
        request.setValue("Bearer " + token, forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        if let phoneSecret { request.setValue(try InvitationWire.secret(phoneSecret), forHTTPHeaderField: "X-Cinema-Phone-Secret") }
        if let grantSecret { request.setValue(try InvitationWire.secret(grantSecret), forHTTPHeaderField: "X-Cinema-Grant-Secret") }
        if var body {
            body["version"] = InvitationWire.version
            let data = try JSONSerialization.data(withJSONObject: body)
            guard data.count <= InvitationWire.maximumBytes else { throw InvitationAPIError(code: "invalid") }
            request.httpBody = data
        }
        let (data, response) = try await InvitationHTTPTransport.read(transport, request: request)
        guard isCurrent, !Task.isCancelled else { throw CancellationError() }
        guard (200..<300).contains(response.statusCode) else { throw InvitationWire.error(data) }
        return data
    }
    func register(id: UUID, name: String, secret: String?) async throws -> (InvitationPhone, String?) {
        let value = try InvitationWire.registration(await request("/phones", body: ["installation_id": id.lowercase, "platform": "apple", "name": RemoteTextBounds.label(name, maximumBytes: 80)], phoneSecret: secret))
        guard value.0.id == id, value.0.platform == "apple", secret == nil ? value.1 != nil : value.1 == nil else { throw InvitationAPIError(code: "invalid") }
        return value
    }
    func availability(phone: InvitationPhone, secret: String, permission: Bool) async throws -> InvitationPhone {
        let value = try InvitationWire.phoneReply(await request("/phones/" + phone.id.lowercase + "/availability",
            body: ["expected_phone_generation": phone.generation, "permission_granted": permission, "resident_active": false], phoneSecret: secret))
        guard value.id == phone.id, value.platform == "apple", value.permissionGranted == permission, !value.residentActive,
              value.generation > phone.generation else { throw InvitationAPIError(code: "invalid") }
        return value
    }
    func rebind(phone: InvitationPhone, secret: String) async throws -> InvitationPhone {
        let value = try InvitationWire.phoneReply(await request("/phones/" + phone.id.lowercase + "/rebind",
            body: ["expected_phone_generation": phone.generation], phoneSecret: secret))
        guard value.id == phone.id, value.platform == "apple", value.generation > phone.generation,
              !value.permissionGranted, !value.residentActive else { throw InvitationAPIError(code: "invalid") }
        return value
    }
    func consent(phone: InvitationPhone, secret: String, receiver: UUID, generation: UInt64, enabled: Bool,
                 grant: RemoteSecretStorage.Grant?) async throws -> InvitationConsent {
        var body: [String: Any] = ["installation_id": phone.id.lowercase, "receiver_id": receiver.lowercase,
                                  "expected_phone_generation": phone.generation, "expected_consent_generation": generation, "enabled": enabled]
        if enabled {
            guard let grant, grant.receiverID == receiver else { throw InvitationAPIError(code: "grant_revoked") }
            body["grant_id"] = grant.id.lowercase; body["transport"] = "apns"
        }
        let value = try InvitationWire.consentReply(await request("/invitations/consent", body: body, phoneSecret: secret, grantSecret: enabled ? grant?.secret : nil))
        guard value.receiverID == receiver, value.enabled == enabled,
              value.generation > generation || (!enabled && generation == 0 && value.generation == 0),
              !enabled || (value.grantID == grant?.id && value.transport == "apns") else { throw InvitationAPIError(code: "invalid") }
        return value
    }
    func consents(phone: InvitationPhone, secret: String) async throws -> [InvitationConsent] {
        var cursor: UUID?, rows: [InvitationConsent] = [], seen = Set<UUID>()
        repeat {
            let (page, next) = try InvitationWire.consentPage(await request("/invitations/consents/list",
                body: ["installation_id": phone.id.lowercase, "after_receiver_id": cursor.map { $0.lowercase } ?? NSNull() as Any, "limit": 20], phoneSecret: secret))
            guard rows.count + page.count <= InvitationWire.maximumConsents,
                  page.allSatisfy({ seen.insert($0.receiverID).inserted }),
                  next == nil || (next == page.last?.receiverID && next != cursor) else { throw InvitationAPIError(code: "invalid") }
            rows += page; cursor = next
        } while cursor != nil
        return rows
    }
    func start(phone: InvitationPhone, secret: String, consent: InvitationConsent, grant: RemoteSecretStorage.Grant) async throws -> (InvitationConsent, InvitationTicket?) {
        guard consent.enabled, consent.receiverID == grant.receiverID, consent.grantID == grant.id, consent.transport == "apns" else { throw InvitationAPIError(code: "grant_revoked") }
        let value = try InvitationWire.transportStart(await request("/invitations/transport/start",
            body: ["installation_id": phone.id.lowercase, "receiver_id": grant.receiverID.lowercase, "grant_id": grant.id.lowercase,
                   "expected_phone_generation": phone.generation, "expected_consent_generation": consent.generation],
            phoneSecret: secret, grantSecret: grant.secret))
        guard value.0.receiverID == consent.receiverID, value.0.grantID == grant.id, value.0.enabled, value.0.transport == "apns",
              value.0.generation > consent.generation, value.0.transportGeneration > consent.transportGeneration else { throw InvitationAPIError(code: "invalid") }
        return value
    }
    func confirm(phone: InvitationPhone, secret: String, consent: InvitationConsent, ticket: InvitationTicket, grant: RemoteSecretStorage.Grant) async throws -> InvitationConsent {
        guard consent.enabled, consent.transport == "apns", consent.receiverID == grant.receiverID, consent.grantID == grant.id else { throw InvitationAPIError(code: "grant_revoked") }
        let value = try InvitationWire.consentReply(await request("/invitations/transport/confirm",
            body: ["installation_id": phone.id.lowercase, "receiver_id": consent.receiverID.lowercase, "ticket_id": ticket.id.lowercase,
                   "expected_phone_generation": phone.generation, "expected_consent_generation": consent.generation,
                   "expected_transport_generation": consent.transportGeneration], phoneSecret: secret, grantSecret: grant.secret))
        guard value.receiverID == consent.receiverID, value.grantID == consent.grantID, value.enabled == consent.enabled,
              value.generation == consent.generation, value.transport == consent.transport, value.transportGeneration == consent.transportGeneration else { throw InvitationAPIError(code: "invalid") }
        return value
    }
    func lookup(phone: InvitationPhone, secret: String, invitationID: String) async throws -> InvitationLookup {
        guard InvitationWire.opaqueID(invitationID) == phone.id else { throw InvitationAPIError(code: "invalid") }
        return try InvitationWire.lookup(await request("/invitations/lookup", body: ["installation_id": phone.id.lowercase, "invitation_id": invitationID], phoneSecret: secret))
    }
    func phones() async throws -> [InvitationPhone] {
        var cursor: UUID?, rows: [InvitationPhone] = [], seen = Set<UUID>()
        repeat {
            let (page, next) = try InvitationWire.phonePage(await request("/phones/list", body: ["after_id": cursor.map { $0.lowercase } ?? NSNull() as Any, "limit": 20]))
            guard rows.count + page.count <= 20, page.allSatisfy({ seen.insert($0.id).inserted }),
                  next == nil || (next == page.last?.id && next != cursor) else { throw InvitationAPIError(code: "invalid") }
            rows += page; cursor = next
        } while cursor != nil
        return rows
    }
    func remove(id: UUID) async throws { try InvitationWire.deleted(await request("/phones/" + id.lowercase, body: nil, method: "DELETE")) }
}

/// There is no home credential field in a direct broker claim owner.
struct InvitationBrokerClaim {
    var transport = InvitationHTTPTransport.session
    func claim(ticket: InvitationTicket, deviceToken: String) async throws {
        let origin = try InvitationWire.brokerOrigin(ticket.brokerOrigin)
        guard !deviceToken.isEmpty, deviceToken.utf8.count <= 512, deviceToken.utf8.count.isMultiple(of: 2),
              deviceToken.utf8.allSatisfy({ (48...57).contains($0) || (65...70).contains($0) || (97...102).contains($0) }),
              let url = URL(string: origin + "/broker/v1/tickets/claim") else { throw InvitationAPIError(code: "invalid") }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"; request.timeoutInterval = 20; request.cachePolicy = .reloadIgnoringLocalCacheData
        request.setValue("Bearer " + (try InvitationWire.secret(ticket.secret)), forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONSerialization.data(withJSONObject: ["version": InvitationWire.version, "ticket_id": ticket.id.lowercase, "platform": "apple", "device_token": deviceToken])
        let (data, response) = try await InvitationHTTPTransport.read(transport, request: request)
        try Self.generation(response, expected: ticket.brokerGeneration)
        guard (200..<300).contains(response.statusCode) else { throw InvitationWire.error(data) }
        try InvitationWire.claimed(data)
    }
    static func generation(_ response: HTTPURLResponse, expected: UUID) throws {
        let fields = response.allHeaderFields.filter { ($0.key as? String)?.lowercased() == "x-cinema-broker-generation" }
        guard fields.count == 1, let text = fields.first?.value as? String,
              (try? InvitationWire.canonicalUUID(text)) == expected else { throw InvitationAPIError(code: "restore_fence_required") }
    }
}
private final class InvitationRedirectBlocker: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) { completionHandler(nil) }
    func urlSession(_ session: URLSession, task: URLSessionTask, didReceive challenge: URLAuthenticationChallenge, completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        completionHandler(challenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust ? .performDefaultHandling : .cancelAuthenticationChallenge, nil)
    }
}
enum InvitationHTTPTransport {
    private static let blocker = InvitationRedirectBlocker()
    static let session: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.urlCache = nil; configuration.urlCredentialStorage = nil; configuration.httpShouldSetCookies = false
        configuration.timeoutIntervalForRequest = 25; configuration.timeoutIntervalForResource = 30
        return URLSession(configuration: configuration, delegate: blocker, delegateQueue: nil)
    }()
    static func read(_ session: URLSession, request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        let (bytes, response) = try await session.bytes(for: request)
        guard let http = response as? HTTPURLResponse else { throw InvitationAPIError(code: "unavailable") }
        var data = Data()
        for try await byte in bytes {
            guard data.count < InvitationWire.maximumBytes else { throw InvitationAPIError(code: "invalid") }
            data.append(byte)
        }
        guard !Task.isCancelled else { throw CancellationError() }
        return (data, http)
    }
}
private extension UUID { var lowercase: String { uuidString.lowercased() } }
