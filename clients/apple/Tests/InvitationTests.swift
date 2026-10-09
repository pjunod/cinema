import Foundation
import XCTest
import Network
@testable import plurx

private final class InvitationTestHTTP: URLProtocol {
    static var answer: ((URLRequest) throws -> (Int, [String: String], Data))!
    static var hold: ((InvitationTestHTTP) -> Bool)?
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        if Self.hold?(self) == true { return }
        resumeResponse()
    }
    func resumeResponse() {
        do {
            let (status, headers, body) = try Self.answer(request)
            client?.urlProtocol(self, didReceive: HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: "HTTP/1.1", headerFields: headers)!, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: body); client?.urlProtocolDidFinishLoading(self)
        } catch { client?.urlProtocol(self, didFailWithError: error) }
    }
    override func stopLoading() {}
}
@MainActor
final class InvitationTests: XCTestCase {
    private let phoneID = UUID(uuidString: "11111111-1111-4111-8111-111111111111")!
    private let receiverID = UUID(uuidString: "22222222-2222-4222-8222-222222222222")!
    private let grantID = UUID(uuidString: "33333333-3333-4333-8333-333333333333")!
    private let ticketID = UUID(uuidString: "44444444-4444-4444-8444-444444444444")!
    private let brokerGeneration = UUID(uuidString: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")!
    private var secret: String { InvitationWire.base64URL(Data(repeating: 7, count: 32)) }
    private var session: URLSession {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [InvitationTestHTTP.self]
        return URLSession(configuration: configuration)
    }
    override func setUp() async throws { Session.shared.setCredentials(origin: "https://home.test", token: "synthetic-home-token") }
    override func tearDown() async throws { InvitationTestHTTP.answer = nil; InvitationTestHTTP.hold = nil; Session.shared.setCredentials(origin: "", token: nil) }
    private func bytes(_ value: Any) throws -> Data { try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]) }
    private func body(_ request: URLRequest) throws -> [String: Any] {
        var data = request.httpBody ?? Data()
        if data.isEmpty, let stream = request.httpBodyStream {
            stream.open(); defer { stream.close() }
            var buffer = [UInt8](repeating: 0, count: 4096)
            while stream.hasBytesAvailable {
                let count = stream.read(&buffer, maxLength: buffer.count)
                guard count >= 0 else { throw InvitationAPIError(code: "invalid") }
                if count == 0 { break }; data.append(contentsOf: buffer.prefix(count))
            }
        }
        return try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
    }
    private func phoneWire(generation: UInt64 = 1) -> [String: Any] {
        ["installation_id": phoneID.uuidString.lowercased(), "name": "Phone 👩‍💻", "platform": "apple", "phone_generation": generation,
         "created_at": 1, "permission_granted": true, "resident_active": false]
    }
    private func phone() throws -> InvitationPhone { try InvitationWire.phone(phoneWire()) }
    private func consentWire(receiver: UUID? = nil, enabled: Bool = true, generation: UInt64 = 1, transportGeneration: UInt64 = 1, transport: String? = "apns") -> [String: Any] {
        ["receiver_id": (receiver ?? receiverID).uuidString.lowercased(), "grant_id": enabled ? grantID.uuidString.lowercased() : NSNull() as Any,
         "enabled": enabled, "transport": enabled ? transport as Any : NSNull(),
         "consent_generation": generation, "transport_generation": transportGeneration,
         "readiness": ["eligible": false, "status": enabled ? "transport_pending" : "disabled", "provider_delivery_verified": false]]
    }
    private func api() -> HomeInvitationAPI {
        var value = HomeInvitationAPI(origin: "https://home.test", token: "synthetic-home-token", authorizationGeneration: Session.shared.playbackAuthorization.generation)
        value.transport = session; return value
    }
    func testStrictInvitationEnvelopesRejectUnknownFieldsAndNoncanonicalNumbersProofs() throws {
        let valid: [String: Any] = ["version": InvitationWire.version, "phone": phoneWire(), "phone_secret": secret]
        XCTAssertEqual(try InvitationWire.registration(bytes(valid)).0.id, phoneID)
        var extra = valid; extra["broker_origin"] = "https://evil.test"
        XCTAssertThrowsError(try InvitationWire.registration(bytes(extra)))
        var nested = phoneWire(); nested["extra"] = true
        extra = valid; extra["phone"] = nested
        XCTAssertThrowsError(try InvitationWire.registration(bytes(extra)))
        let text = String(decoding: try bytes(valid), as: UTF8.self)
        for invalid in [text.replacingOccurrences(of: "\"phone_generation\":1", with: "\"phone_generation\":1.0"),
                        text.replacingOccurrences(of: "\"created_at\":1", with: "\"created_at\":-0"),
                        text.replacingOccurrences(of: "\"version\":", with: "\"version\":\"cinema.invitation.v1\",\"version\":")] {
            XCTAssertThrowsError(try InvitationWire.registration(Data(invalid.utf8)))
        }
        extra = valid; nested = phoneWire(); nested["permission_granted"] = 1; extra["phone"] = nested
        XCTAssertThrowsError(try InvitationWire.registration(bytes(extra)))
        var noncanonical = secret; noncanonical.removeLast(); noncanonical.append("d")
        XCTAssertThrowsError(try InvitationWire.secret(noncanonical))
        extra = valid; extra["phone_secret"] = NSNull()
        XCTAssertNil(try InvitationWire.registration(bytes(extra)).1)
    }
    func testTicketRejectsUntrustedOriginOuterDialectAndGeneration() throws {
        var ticket: [String: Any] = ["ticket_id": ticketID.uuidString.lowercased(), "ticket_secret": secret, "expires_at": 120,
                                    "broker_origin": "https://broker.test", "broker_generation": brokerGeneration.uuidString.lowercased()]
        var value: [String: Any] = ["version": InvitationWire.version, "consent": consentWire(), "ticket": ticket]
        XCTAssertEqual(try InvitationWire.transportStart(bytes(value)).1?.brokerGeneration, brokerGeneration)
        for origin in ["http://broker.test", "https://broker.test/", "https://user@broker.test", "https://broker.test?proof=bad", "https://broker.test#bad"] {
            ticket["broker_origin"] = origin; value["ticket"] = ticket
            XCTAssertThrowsError(try InvitationWire.transportStart(bytes(value)))
        }
        ticket["broker_origin"] = "https://broker.test"; ticket["broker_generation"] = brokerGeneration.uuidString
        value["ticket"] = ticket
        XCTAssertThrowsError(try InvitationWire.transportStart(bytes(value)))
        ticket["broker_generation"] = brokerGeneration.uuidString.lowercased(); value["ticket"] = ticket
        value["broker_origin"] = "https://broker.test"
        XCTAssertThrowsError(try InvitationWire.transportStart(bytes(value)))
    }
    func testConsentPaginationKeepsMoreThanOnePageAndBoundsRepeatedCursor() async throws {
        var requestCount = 0
        let receivers = (1...21).map { UUID(uuidString: String(format: "%08x-0000-4000-8000-000000000000", $0))! }
        InvitationTestHTTP.answer = { request in
            requestCount += 1
            let requestBody = try self.body(request)
            XCTAssertEqual(request.value(forHTTPHeaderField: "X-Cinema-Phone-Secret"), self.secret)
            XCTAssertNil(request.value(forHTTPHeaderField: "X-Cinema-Grant-Secret"))
            let page = requestCount == 1 ? Array(receivers.prefix(20)) : [receivers[20]]
            if requestCount == 2 { XCTAssertEqual(requestBody["after_receiver_id"] as? String, receivers[19].uuidString.lowercased()) }
            return (200, [:], try self.bytes(["version": InvitationWire.version, "consents": page.map { self.consentWire(receiver: $0) },
                                              "next_cursor": requestCount == 1 ? receivers[19].uuidString.lowercased() : NSNull() as Any]))
        }
        let rows = try await api().consents(phone: phone(), secret: secret)
        XCTAssertEqual(rows.count, 21); XCTAssertEqual(requestCount, 2)
        requestCount = 0
        InvitationTestHTTP.answer = { _ in
            requestCount += 1
            return (200, [:], try self.bytes(["version": InvitationWire.version, "consents": [self.consentWire()],
                                              "next_cursor": self.receiverID.uuidString.lowercased()]))
        }
        do { _ = try await api().consents(phone: phone(), secret: secret); XCTFail("Repeated cursor accepted") } catch {}
        XCTAssertEqual(requestCount, 2)
    }
    func testInvitationOffDoesNotRequireGrantOrForegroundRemoteMaster() async throws {
        InvitationTestHTTP.answer = { request in
            let sent = try self.body(request)
            XCTAssertEqual(sent["enabled"] as? Bool, false)
            XCTAssertNil(sent["grant_id"]); XCTAssertNil(sent["transport"])
            XCTAssertNil(request.value(forHTTPHeaderField: "X-Cinema-Grant-Secret"))
            return (200, [:], try self.bytes(["version": InvitationWire.version, "consent": self.consentWire(enabled: false)]))
        }
        let result = try await api().consent(phone: phone(), secret: secret, receiver: receiverID, generation: 0, enabled: false, grant: nil)
        XCTAssertFalse(result.enabled)
    }
    func testBrokerClaimOnlySendsTicketProofAndRejectsInvalidAppleToken() async throws {
        let ticket = InvitationTicket(id: ticketID, secret: secret, expiresAt: 120, brokerOrigin: "https://broker.test", brokerGeneration: brokerGeneration)
        var called = 0
        InvitationTestHTTP.answer = { request in
            called += 1
            XCTAssertEqual(request.url?.absoluteString, "https://broker.test/broker/v1/tickets/claim")
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer " + self.secret)
            XCTAssertFalse(request.allHTTPHeaderFields?.keys.contains(where: { $0.lowercased().hasPrefix("x-cinema-") }) == true)
            let sent = try self.body(request)
            XCTAssertEqual(Set(sent.keys), ["version", "ticket_id", "platform", "device_token"])
            XCTAssertEqual(sent["platform"] as? String, "apple")
            return (200, ["X-Cinema-Broker-Generation": self.brokerGeneration.uuidString.lowercased()], try self.bytes(["version": InvitationWire.version, "status": "claimed"]))
        }
        var broker = InvitationBrokerClaim(); broker.transport = session
        try await broker.claim(ticket: ticket, deviceToken: "0a" + String(repeating: "00", count: 31))
        XCTAssertEqual(called, 1)
        for token in ["", "a", "zz", String(repeating: "0", count: 514)] {
            do { try await broker.claim(ticket: ticket, deviceToken: token); XCTFail("Bad APNs token accepted") } catch {}
        }
        XCTAssertEqual(called, 1)
    }
    func testBrokerGenerationHeaderMustBeSingleCanonicalAndIssued() throws {
        let origin = URL(string: "https://broker.test")!
        for headers in [[:], ["X-Cinema-Broker-Generation": brokerGeneration.uuidString],
                        ["X-Cinema-Broker-Generation": receiverID.uuidString.lowercased()],
                        ["X-Cinema-Broker-Generation": brokerGeneration.uuidString.lowercased() + ", " + brokerGeneration.uuidString.lowercased()]] {
            let response = HTTPURLResponse(url: origin, statusCode: 200, httpVersion: "HTTP/1.1", headerFields: headers)!
            XCTAssertThrowsError(try InvitationBrokerClaim.generation(response, expected: brokerGeneration))
        }
    }
    func testConfirmRefusesReplacementConsentGenerationTransportAndMismatchedGrant() async throws {
        let consent = try InvitationWire.consent(consentWire())
        let ticket = InvitationTicket(id: ticketID, secret: secret, expiresAt: 120, brokerOrigin: "https://broker.test", brokerGeneration: brokerGeneration)
        let grant = RemoteSecretStorage.Grant(receiverID: receiverID, id: grantID, secret: secret)
        for replacement in [consentWire(generation: 2), consentWire(transport: "fcm")] {
            InvitationTestHTTP.answer = { _ in (200, [:], try self.bytes(["version": InvitationWire.version, "consent": replacement])) }
            do { _ = try await api().confirm(phone: phone(), secret: secret, consent: consent, ticket: ticket, grant: grant); XCTFail("Replacement confirmed") } catch {}
        }
        var called = false
        InvitationTestHTTP.answer = { _ in called = true; throw InvitationAPIError(code: "invalid") }
        let wrong = RemoteSecretStorage.Grant(receiverID: phoneID, id: grantID, secret: secret)
        do { _ = try await api().confirm(phone: phone(), secret: secret, consent: consent, ticket: ticket, grant: wrong); XCTFail("Wrong grant sent") } catch {}
        XCTAssertFalse(called)
    }
    func testProvedRegistrationCannotSilentlyRotateMissingInstallationProof() async throws {
        var calls = 0
        InvitationTestHTTP.answer = { request in
            calls += 1
            XCTAssertEqual(request.value(forHTTPHeaderField: "X-Cinema-Phone-Secret"), self.secret)
            return (200, [:], try self.bytes(["version": InvitationWire.version, "phone": self.phoneWire(), "phone_secret": self.secret]))
        }
        do { _ = try await api().register(id: phoneID, name: "Phone", secret: secret); XCTFail("Fresh one-time proof silently accepted") } catch {}
        XCTAssertEqual(calls, 1)
    }
    func testStartRejectsReplacementTransportBeforeClaimingProvider() async throws {
        let consent = try InvitationWire.consent(consentWire())
        let grant = RemoteSecretStorage.Grant(receiverID: receiverID, id: grantID, secret: secret)
        InvitationTestHTTP.answer = { _ in
            (200, [:], try self.bytes(["version": InvitationWire.version,
                "consent": self.consentWire(generation: 2, transportGeneration: 2, transport: "fcm"), "ticket": NSNull()]))
        }
        do { _ = try await api().start(phone: phone(), secret: secret, consent: consent, grant: grant); XCTFail("Replacement transport accepted") } catch {}
    }

    func testActualURLSessionRejectsSameAndDifferentlyCasedDuplicateGenerationHeaders() async throws {
        let server = try InvitationHeaderServer()
        defer { server.stop() }
        let port = try await server.start()
        for path in ["valid", "same", "case"] {
            let url = try XCTUnwrap(URL(string: "http://127.0.0.1:\(port)/\(path)"))
            let (_, response) = try await InvitationHTTPTransport.read(InvitationHTTPTransport.session, request: URLRequest(url: url))
            if path == "valid" {
                XCTAssertNoThrow(try InvitationBrokerClaim.generation(response, expected: brokerGeneration))
            } else {
                XCTAssertThrowsError(try InvitationBrokerClaim.generation(response, expected: brokerGeneration))
                let value = response.value(forHTTPHeaderField: "X-Cinema-Broker-Generation")
                XCTAssertTrue(value?.contains(",") == true)
            }
        }
    }
    func testContradictoryEligibleAndZeroEnabledConsentCannotEnterCache() throws {
        var value = consentWire(enabled: false)
        value["readiness"] = ["eligible": true, "status": "disabled", "provider_delivery_verified": false]
        XCTAssertThrowsError(try InvitationWire.consent(value))
        value = consentWire()
        value["readiness"] = ["eligible": true, "status": "transport_pending", "provider_delivery_verified": false]
        XCTAssertThrowsError(try InvitationWire.consent(value))
        value = consentWire(generation: 0)
        XCTAssertThrowsError(try InvitationWire.consent(value))
        value = consentWire(enabled: false, generation: 0, transportGeneration: 0)
        XCTAssertNoThrow(try InvitationWire.consent(value))
        value = consentWire(enabled: false, generation: 3, transportGeneration: 2)
        value["grant_id"] = grantID.uuidString.lowercased(); value["transport"] = "apns"
        XCTAssertNoThrow(try InvitationWire.consent(value))
    }

}

private final class InvitationHeaderOnce: @unchecked Sendable {
    private let lock = NSLock()
    private var finished = false
    func finish(_ action: () -> Void) {
        lock.lock()
        guard !finished else { lock.unlock(); return }
        finished = true; lock.unlock(); action()
    }
}
/// Raw loopback HTTP deliberately sends duplicate lines rather than constructing
/// a Foundation response dictionary, which normalizes duplicates before admission.
private final class InvitationHeaderServer: @unchecked Sendable {
    private let listener: NWListener
    private let queue = DispatchQueue(label: "cinema.invitation.headers.fixture")
    init() throws { listener = try NWListener(using: .tcp, on: .any) }
    func start() async throws -> UInt16 {
        try await withCheckedThrowingContinuation { continuation in
            let once = InvitationHeaderOnce()
            listener.stateUpdateHandler = { [weak self] state in
                switch state {
                case .ready:
                    if let port = self?.listener.port?.rawValue { once.finish { continuation.resume(returning: port) } }
                case .failed(let error): once.finish { continuation.resume(throwing: error) }
                case .cancelled: once.finish { continuation.resume(throwing: CancellationError()) }
                default: break
                }
            }
            listener.newConnectionHandler = { [queue] connection in
                connection.start(queue: queue)
                connection.receive(minimumIncompleteLength: 1, maximumLength: 8192) { data, _, _, _ in
                    let request = String(decoding: data ?? Data(), as: UTF8.self)
                    var headers = "X-Cinema-Broker-Generation: aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa\r\n"
                    if request.hasPrefix("GET /same ") {
                        headers += "X-Cinema-Broker-Generation: aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa\r\n"
                    } else if request.hasPrefix("GET /case ") {
                        headers += "x-cinema-broker-generation: 22222222-2222-4222-8222-222222222222\r\n"
                    }
                    let body = "{\"version\":\"cinema.invitation.v1\",\"status\":\"claimed\"}"
                    let response = "HTTP/1.1 200 OK\r\n" + headers + "Content-Type: application/json\r\nContent-Length: \(body.utf8.count)\r\nConnection: close\r\n\r\n" + body
                    connection.send(content: Data(response.utf8), completion: .contentProcessed { _ in connection.cancel() })
                }
            }
            listener.start(queue: queue)
        }
    }
    func stop() { listener.cancel() }
}

private final class InvitationMemoryVault: TokenStoring {
    var value: String?
    func read() -> String? { value }
    func write(_ token: String) -> Bool { value = token; return true }
    func clear() { value = nil }
}

extension InvitationTests {
    func testNotificationRollbackOffAndRetiredInstallationCannotRedisplayOrEnroll() async throws {
        let suite = "invitation-tests-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let vault = InvitationMemoryVault()
        let store = InvitationSecureState(defaults: defaults, vault: { _ in vault })
        let identity = InvitationIdentity(origin: "https://home.test", instance: "test-instance", userID: 1)
        var profile = try store.create(identity, name: "Phone")
        profile = try store.choose(true, receiverID: receiverID, name: "TV", profile: profile)
        let auth = InvitationNotificationIdentity(identity: identity, generation: 1, fingerprint: "bound-login")
        var current: InvitationNotificationIdentity? = auth
        var now: TimeInterval = 100
        var consent = consentWire()
        consent["readiness"] = ["eligible": true, "status": "ready", "provider_delivery_verified": false]
        let ready = try InvitationWire.consent(consent)
        let phone = InvitationPhone(id: profile.installationID, name: "Phone", platform: "apple", generation: 1, createdAt: 1, permissionGranted: true, residentActive: false)
        var proof = InvitationProofRecord(identity: identity, installationID: profile.installationID)
        proof.phone = phone; proof.secret = secret; proof.loginFingerprint = auth.fingerprint
        try store.mergeConsents([ready], record: &proof, profile: profile)
        let bridge = InvitationNotificationBridge(store: store, defaults: defaults, clock: { now }, authorization: { current }, permission: { true })
        bridge.activeIdentity = identity
        var idBytes = profile.installationID.uuid
        var opaque = withUnsafeBytes(of: &idBytes) { Data($0) }; opaque.append(Data(repeating: 3, count: 16))
        let id = InvitationWire.base64URL(opaque)
        let firstVisible = await bridge.shouldPresent(category: InvitationNotificationBridge.category, invitationID: id)
        XCTAssertTrue(firstVisible)
        now = 90
        let refused = await bridge.shouldPresent(category: InvitationNotificationBridge.category, invitationID: id)
        XCTAssertFalse(refused)
        now = .infinity
        XCTAssertFalse(bridge.acceptDefaultTap(category: InvitationNotificationBridge.category, invitationID: id, defaultAction: true))
        now = 101
        bridge.requestRegistration(owner: auth)
        bridge.registered(Data([1, 2]))
        XCTAssertEqual(bridge.deviceToken, "0102")
        bridge.registered(Data([3, 4])) // OS rotation has no new request identifier.
        XCTAssertEqual(bridge.deviceToken, "0304")
        profile = try store.choose(false, receiverID: receiverID, name: "TV", profile: profile)
        bridge.registered(Data([5, 6]))
        XCTAssertEqual(bridge.deviceToken, "0304")
        let offVisible = await bridge.shouldPresent(category: InvitationNotificationBridge.category, invitationID: id)
        XCTAssertFalse(offVisible)
        current = .init(identity: InvitationIdentity(origin: identity.origin, instance: identity.instance, userID: 2), generation: 2, fingerprint: "another-login")
        bridge.activeIdentity = current?.identity
        bridge.registered(Data([7, 8]))
        XCTAssertEqual(bridge.deviceToken, "0304")
        profile = try store.disableAll(profile, removal: true)
        XCTAssertFalse(bridge.acceptDefaultTap(category: InvitationNotificationBridge.category, invitationID: id, defaultAction: true))
    }
    func testConsentRefreshCannotOverrideNewerOfflineOffOrRetainMissingServerAuthority() throws {
        let suite = "invitation-tests-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite)); defer { defaults.removePersistentDomain(forName: suite) }
        let vault = InvitationMemoryVault(), store = InvitationSecureState(defaults: defaults, vault: { _ in vault })
        let identity = InvitationIdentity(origin: "https://home.test", instance: "instance", userID: 1)
        var profile = try store.create(identity, name: "Phone")
        profile = try store.choose(true, receiverID: receiverID, name: "TV", profile: profile)
        let oldIntent = try XCTUnwrap(profile.choices[receiverID.uuidString.lowercased()]).intent
        var proof = InvitationProofRecord(identity: identity, installationID: profile.installationID)
        let on = try InvitationWire.consent(consentWire())
        _ = try store.choose(false, receiverID: receiverID, name: "TV", profile: profile)
        try store.mergeConsents([on], record: &proof, profile: profile)
        try store.acknowledge(on, intent: oldIntent, profile: profile)
        XCTAssertEqual(try store.profile(identity)?.choices[receiverID.uuidString.lowercased()]?.enabled, false)
        XCTAssertEqual(try store.profile(identity)?.choices[receiverID.uuidString.lowercased()]?.pending, true)
        try store.mergeConsents([], record: &proof, profile: profile)
        XCTAssertTrue(proof.consents.isEmpty)
    }
}

extension InvitationTests {
    func testRemoteGrantProofRequiresExactCanonicalOriginInstanceAndAccount() throws {
        let first = try XCTUnwrap(RemoteSecretStorage.scopedIdentity(origin: "https://first.test", instance: "copied-instance", userID: 1))
        let other = try XCTUnwrap(RemoteSecretStorage.scopedIdentity(origin: "https://other.test", instance: "copied-instance", userID: 1))
        XCTAssertNotEqual(first, other)
        XCTAssertNotEqual(first, "copied-instance:1")
        var vaults: [String: InvitationMemoryVault] = [:]
        let factory: (String) -> any TokenStoring = { key in
            if let found = vaults[key] { return found }
            let created = InvitationMemoryVault(); vaults[key] = created; return created
        }
        let a = RemoteSecretStorage(identity: first, vault: factory)
        let b = RemoteSecretStorage(identity: other, vault: factory)
        try a.saveGrant(.init(receiverID: receiverID, id: grantID, secret: secret))
        XCTAssertEqual(a.grants.first?.id, grantID)
        XCTAssertTrue(b.grants.isEmpty)
        XCTAssertTrue(RemoteSecretStorage(identity: "copied-instance:1", vault: factory).grants.isEmpty)
    }
}

extension InvitationTests {
    private func settle(_ model: InvitationClientModel) async throws {
        let deadline = ProcessInfo.processInfo.systemUptime + 5
        while model.busy && ProcessInfo.processInfo.systemUptime < deadline { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertFalse(model.busy)
    }
    func testProductionModelDrainsOfflineOffBeforeLoginRebindAndRevokedOn() async throws {
        let suite = "invitation-model-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite)); defer { defaults.removePersistentDomain(forName: suite) }
        let vault = InvitationMemoryVault(), store = InvitationSecureState(defaults: defaults, vault: { _ in vault })
        let identity = InvitationIdentity(origin: "https://home.test", instance: "instance", userID: 1)
        var profile = try store.create(identity, name: "Phone")
        let offReceiver = UUID(uuidString: "55555555-5555-4555-8555-555555555555")!
        profile = try store.choose(true, receiverID: receiverID, name: "Revoked ON", profile: profile)
        profile = try store.choose(false, receiverID: offReceiver, name: "Offline OFF", profile: profile)
        let owner = InvitationNotificationIdentity(identity: identity, generation: Session.shared.playbackAuthorization.generation, fingerprint: "new-login")
        var wire = phoneWire(); wire["installation_id"] = profile.installationID.uuidString.lowercased()
        var proof = InvitationProofRecord(identity: identity, installationID: profile.installationID)
        proof.phone = try InvitationWire.phone(wire); proof.secret = secret; proof.loginFingerprint = "old-login"; proof.registrationAttempted = true
        try store.saveProof(proof, profile: profile)
        var paths: [String] = []
        InvitationTestHTTP.answer = { request in
            paths.append(request.url!.path)
            switch request.url!.path {
            case "/api/remote/v1/phones":
                return (200, [:], try self.bytes(["version": InvitationWire.version, "phone": wire, "phone_secret": NSNull()]))
            case "/api/remote/v1/invitations/consents/list":
                return (200, [:], try self.bytes(["version": InvitationWire.version, "consents": [self.consentWire(receiver: offReceiver)], "next_cursor": NSNull()]))
            case "/api/remote/v1/invitations/consent":
                let sent = try self.body(request)
                XCTAssertEqual(sent["receiver_id"] as? String, offReceiver.uuidString.lowercased())
                XCTAssertEqual(sent["enabled"] as? Bool, false)
                XCTAssertNil(request.value(forHTTPHeaderField: "X-Cinema-Grant-Secret"))
                return (200, [:], try self.bytes(["version": InvitationWire.version, "consent": self.consentWire(receiver: offReceiver, enabled: false, generation: 2)]))
            default: XCTFail("Unexpected model request"); throw InvitationAPIError(code: "invalid")
            }
        }
        let bridge = InvitationNotificationBridge(store: store, defaults: defaults, authorization: { owner }, permission: { false })
        let model = InvitationClientModel(store: store, bridge: bridge, authorization: { owner }, permission: { false }, requestPermission: { XCTFail("OFF must not ask permission"); return false }, grantLookup: { _, _ in nil }, makeAPI: { _ in self.api() })
        model.configure(active: true, remoteEnabled: false)
        try await settle(model)
        XCTAssertEqual(paths, ["/api/remote/v1/phones", "/api/remote/v1/invitations/consents/list", "/api/remote/v1/invitations/consent"])
        XCTAssertEqual(try store.profile(identity)?.choices[offReceiver.uuidString.lowercased()]?.pending, false)
        XCTAssertEqual(try store.profile(identity)?.choices[receiverID.uuidString.lowercased()]?.enabled, true)
        XCTAssertTrue(model.requiresRebind)
    }
    func testProductionRebindRecoversUnknownPhoneGenerationBeforeCas() async throws {
        let suite = "invitation-model-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite)); defer { defaults.removePersistentDomain(forName: suite) }
        let vault = InvitationMemoryVault(), store = InvitationSecureState(defaults: defaults, vault: { _ in vault })
        let identity = InvitationIdentity(origin: "https://home.test", instance: "instance", userID: 1)
        let profile = try store.create(identity, name: "Phone")
        let owner = InvitationNotificationIdentity(identity: identity, generation: Session.shared.playbackAuthorization.generation, fingerprint: "new-login")
        var wire = phoneWire(); wire["installation_id"] = profile.installationID.uuidString.lowercased()
        var proof = InvitationProofRecord(identity: identity, installationID: profile.installationID)
        proof.phone = try InvitationWire.phone(wire); proof.secret = secret; proof.loginFingerprint = "old-login"; proof.registrationAttempted = true; proof.metadataNeedsRefresh = true
        try store.saveProof(proof, profile: profile)
        var paths: [String] = []
        InvitationTestHTTP.answer = { request in
            paths.append(request.url!.path)
            if request.url!.path == "/api/remote/v1/phones" {
                var fresh = wire; fresh["phone_generation"] = 7
                return (200, [:], try self.bytes(["version": InvitationWire.version, "phone": fresh, "phone_secret": NSNull()]))
            }
            XCTAssertTrue(request.url!.path.hasSuffix("/rebind"))
            XCTAssertEqual(try self.body(request)["expected_phone_generation"] as? Int, 7)
            var rebound = wire; rebound["phone_generation"] = 8; rebound["permission_granted"] = false
            return (200, [:], try self.bytes(["version": InvitationWire.version, "phone": rebound]))
        }
        let bridge = InvitationNotificationBridge(store: store, defaults: defaults, authorization: { owner }, permission: { false })
        let model = InvitationClientModel(store: store, bridge: bridge, authorization: { owner }, permission: { false }, grantLookup: { _, _ in nil }, makeAPI: { _ in self.api() })
        model.configure(active: true, remoteEnabled: false)
        model.rebind()
        try await settle(model)
        XCTAssertEqual(paths.count, 2)
        XCTAssertEqual(try store.proof(profile)?.phone?.generation, 8)
        XCTAssertEqual(try store.proof(profile)?.loginFingerprint, owner.fingerprint)
        XCTAssertFalse(model.requiresRebind)
    }
}

private final class InvitationRequestLatch: @unchecked Sendable {
    private let lock = NSLock()
    private var held: InvitationTestHTTP?
    private var captured = false
    private var paths: [String] = []
    private let path: String
    init(path: String = "/api/remote/v1/phones") { self.path = path }
    func holdFirstRequest(_ request: InvitationTestHTTP) -> Bool {
        lock.withLock {
            guard !captured, request.request.url?.path == path else { return false }
            captured = true; held = request; return true
        }
    }
    var isHeld: Bool { lock.withLock { held != nil } }
    func record(_ path: String) { lock.withLock { paths.append(path) } }
    var receivedPaths: [String] { lock.withLock { paths } }
    func release() { let value = lock.withLock { let value = held; held = nil; return value }; value?.resumeResponse() }
}
extension InvitationTests {
    func testProductionModelCoalescesTokenRotationAndDrainsTapAfterAwaitedReconcile() async throws {
        let suite = "invitation-model-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite)); defer { defaults.removePersistentDomain(forName: suite) }
        let vault = InvitationMemoryVault(), store = InvitationSecureState(defaults: defaults, vault: { _ in vault })
        let identity = InvitationIdentity(origin: "https://home.test", instance: "instance", userID: 1)
        var profile = try store.create(identity, name: "Phone")
        profile = try store.choose(true, receiverID: receiverID, name: "TV", profile: profile)
        let owner = InvitationNotificationIdentity(identity: identity, generation: Session.shared.playbackAuthorization.generation, fingerprint: "login")
        var wire = phoneWire(); wire["installation_id"] = profile.installationID.uuidString.lowercased()
        var readyWire = consentWire(); readyWire["readiness"] = ["eligible": true, "status": "ready", "provider_delivery_verified": false]
        let ready = try InvitationWire.consent(readyWire)
        var proof = InvitationProofRecord(identity: identity, installationID: profile.installationID)
        proof.phone = try InvitationWire.phone(wire); proof.secret = secret; proof.loginFingerprint = owner.fingerprint; proof.registrationAttempted = true
        try store.mergeConsents([ready], record: &proof, profile: profile)
        try store.acknowledge(ready, intent: profile.choices[receiverID.uuidString.lowercased()]!.intent, profile: profile)
        let latch = InvitationRequestLatch()
        InvitationTestHTTP.hold = { latch.holdFirstRequest($0) }
        InvitationTestHTTP.answer = { request in
            let path = request.url!.path; latch.record(path)
            switch path {
            case "/api/remote/v1/phones": return (200, [:], try self.bytes(["version": InvitationWire.version, "phone": wire, "phone_secret": NSNull()]))
            case "/api/remote/v1/invitations/consents/list": return (200, [:], try self.bytes(["version": InvitationWire.version, "consents": [readyWire], "next_cursor": NSNull()]))
            case "/api/remote/v1/invitations/lookup": return (200, [:], try self.bytes(["version": InvitationWire.version, "receiver_id": self.receiverID.uuidString.lowercased(), "foreground_id": self.ticketID.uuidString.lowercased(), "target": ["owner_node_id": "fixture", "session_id": self.ticketID.uuidString.lowercased(), "receiver_epoch": self.grantID.uuidString.lowercased()], "expires_at": 120]))
            default: XCTFail("Unexpected authority effect"); throw InvitationAPIError(code: "invalid")
            }
        }
        let bridge = InvitationNotificationBridge(store: store, defaults: defaults, authorization: { owner }, permission: { true })
        let model = InvitationClientModel(store: store, bridge: bridge, authorization: { owner }, permission: { true }, grantLookup: { _, _ in nil }, makeAPI: { _ in self.api() })
        model.configure(active: true, remoteEnabled: true)
        let deadline = ProcessInfo.processInfo.systemUptime + 5
        while !latch.isHeld && ProcessInfo.processInfo.systemUptime < deadline { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertTrue(latch.isHeld)
        bridge.requestRegistration(owner: owner); bridge.registered(Data([1, 2])); bridge.registered(Data([3, 4]))
        var uuid = profile.installationID.uuid
        var payload = withUnsafeBytes(of: &uuid) { Data($0) }; payload.append(Data(repeating: 5, count: 16))
        XCTAssertTrue(bridge.acceptDefaultTap(category: InvitationNotificationBridge.category, invitationID: InvitationWire.base64URL(payload), defaultAction: true))
        try await Task.sleep(for: .milliseconds(20))
        latch.release()
        while (model.busy || model.tapReady == nil) && ProcessInfo.processInfo.systemUptime < deadline { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertEqual(model.tapReady?.receiverID, receiverID)
        XCTAssertEqual(latch.receivedPaths.filter { $0.hasSuffix("/phones") }.count, 2)
        XCTAssertEqual(latch.receivedPaths.filter { $0.hasSuffix("/lookup") }.count, 1)
        XCTAssertFalse(latch.receivedPaths.contains { $0.contains("transport") || $0.contains("control") })
        bridge.registered(Data([3, 4])) // unchanged token must not restart work.
        try await Task.sleep(for: .milliseconds(50))
        XCTAssertEqual(latch.receivedPaths.filter { $0.hasSuffix("/phones") }.count, 2)
        let handoff = try XCTUnwrap(model.prepareTapHandoff())
        XCTAssertEqual(model.validateTapHandoff(handoff)?.receiverID, receiverID)
        model.configure(active: true, remoteEnabled: false)
        XCTAssertNil(model.validateTapHandoff(handoff)) // sheet dismissal must recheck master.
        model.configure(active: true, remoteEnabled: true)
        model.configure(active: false, remoteEnabled: true)
        XCTAssertNil(model.validateTapHandoff(handoff)) // inactive owner cannot present.
        model.configure(active: true, remoteEnabled: true)
        _ = try store.choose(false, receiverID: receiverID, name: "TV", profile: profile)
        _ = try store.choose(true, receiverID: receiverID, name: "TV", profile: profile)
        XCTAssertNil(model.validateTapHandoff(handoff)) // OFF→ON cannot resurrect old intent.
        model.configure(active: false, remoteEnabled: true)
    }
}

private final class InvitationTestClock: @unchecked Sendable {
    private let lock = NSLock()
    private var value: TimeInterval = 100
    var now: TimeInterval { lock.withLock { value } }
    func set(_ value: TimeInterval) { lock.withLock { self.value = value } }
}
extension InvitationTests {
    func testProductionEnrollmentExpiredStartCannotClaimAndRequiresFreshExplicitRetry() async throws {
        let suite = "invitation-model-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite)); defer { defaults.removePersistentDomain(forName: suite) }
        let vault = InvitationMemoryVault(), store = InvitationSecureState(defaults: defaults, vault: { _ in vault })
        let identity = InvitationIdentity(origin: "https://home.test", instance: "instance", userID: 1)
        var profile = try store.create(identity, name: "Phone")
        profile = try store.choose(true, receiverID: receiverID, name: "TV", profile: profile)
        let owner = InvitationNotificationIdentity(identity: identity, generation: Session.shared.playbackAuthorization.generation, fingerprint: "login")
        var wire = phoneWire(); wire["installation_id"] = profile.installationID.uuidString.lowercased()
        var consent = consentWire()
        var proof = InvitationProofRecord(identity: identity, installationID: profile.installationID)
        proof.phone = try InvitationWire.phone(wire); proof.secret = secret; proof.loginFingerprint = owner.fingerprint; proof.registrationAttempted = true
        try store.mergeConsents([try InvitationWire.consent(consent)], record: &proof, profile: profile)
        try store.acknowledge(try InvitationWire.consent(consent), intent: profile.choices[receiverID.uuidString.lowercased()]!.intent, profile: profile)
        let latch = InvitationRequestLatch(), clock = InvitationTestClock()
        InvitationTestHTTP.hold = { latch.holdFirstRequest($0) }
        var starts = 0
        InvitationTestHTTP.answer = { request in
            let path = request.url!.path; latch.record(path)
            switch path {
            case "/api/remote/v1/phones": return (200, [:], try self.bytes(["version": InvitationWire.version, "phone": wire, "phone_secret": NSNull()]))
            case "/api/remote/v1/invitations/consents/list": return (200, [:], try self.bytes(["version": InvitationWire.version, "consents": [consent], "next_cursor": NSNull()]))
            case "/api/remote/v1/invitations/transport/start":
                starts += 1
                consent["consent_generation"] = starts + 1; consent["transport_generation"] = starts + 1
                if starts == 1 { clock.set(221) }
                let ticket: [String: Any] = ["ticket_id": self.ticketID.uuidString.lowercased(), "ticket_secret": self.secret, "expires_at": 120, "broker_origin": "https://broker.test", "broker_generation": self.brokerGeneration.uuidString.lowercased()]
                return (200, [:], try self.bytes(["version": InvitationWire.version, "consent": consent, "ticket": ticket]))
            case "/broker/v1/tickets/claim":
                XCTAssertNil(request.value(forHTTPHeaderField: "X-Cinema-Phone-Secret")); XCTAssertNil(request.value(forHTTPHeaderField: "X-Cinema-Grant-Secret"))
                XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer " + self.secret)
                return (200, ["X-Cinema-Broker-Generation": self.brokerGeneration.uuidString.lowercased()], try self.bytes(["version": InvitationWire.version, "status": "claimed"]))
            case "/api/remote/v1/invitations/transport/confirm":
                consent["readiness"] = ["eligible": true, "status": "ready", "provider_delivery_verified": false]
                return (200, [:], try self.bytes(["version": InvitationWire.version, "consent": consent]))
            default: XCTFail("Unexpected invitation effect"); throw InvitationAPIError(code: "invalid")
            }
        }
        var broker = InvitationBrokerClaim(); broker.transport = session
        let bridge = InvitationNotificationBridge(store: store, defaults: defaults, authorization: { owner }, permission: { true })
        let model = InvitationClientModel(store: store, bridge: bridge, authorization: { owner }, permission: { true }, grantLookup: { _, _ in .init(receiverID: self.receiverID, id: self.grantID, secret: self.secret) }, makeAPI: { _ in self.api() }, broker: broker, monotonic: { clock.now })
        model.configure(active: true, remoteEnabled: true)
        let deadline = ProcessInfo.processInfo.systemUptime + 5
        while !latch.isHeld && ProcessInfo.processInfo.systemUptime < deadline { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertTrue(latch.isHeld)
        bridge.requestRegistration(owner: owner); bridge.registered(Data([1, 2]))
        try await Task.sleep(for: .milliseconds(20)); latch.release()
        try await settle(model)
        XCTAssertEqual(starts, 1)
        XCTAssertFalse(latch.receivedPaths.contains { $0.hasSuffix("/claim") || $0.hasSuffix("/confirm") })
        XCTAssertEqual(try store.profile(identity)?.choices[self.receiverID.uuidString.lowercased()]?.enabled, true)
        model.retryTransport(receiver: receiverID)
        try await settle(model)
        XCTAssertEqual(starts, 2)
        XCTAssertEqual(latch.receivedPaths.filter { $0.hasSuffix("/claim") }.count, 1)
        XCTAssertEqual(latch.receivedPaths.filter { $0.hasSuffix("/confirm") }.count, 1)
        XCTAssertEqual(model.record?.consents[receiverID.uuidString.lowercased()]?.readiness, .ready)
    }
}

extension InvitationTests {
    func testApnsFailureIsAdvisoryUntilExplicitRetryAndTokenNeverEnablesChoice() throws {
        let suite = "invitation-registration-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite)); defer { defaults.removePersistentDomain(forName: suite) }
        let vault = InvitationMemoryVault(), store = InvitationSecureState(defaults: defaults, vault: { _ in vault })
        let identity = InvitationIdentity(origin: "https://home.test", instance: "instance", userID: 1)
        var profile = try store.create(identity, name: "Phone")
        profile = try store.choose(true, receiverID: receiverID, name: "TV", profile: profile)
        let owner = InvitationNotificationIdentity(identity: identity, generation: 1, fingerprint: "login")
        var wire = phoneWire(); wire["installation_id"] = profile.installationID.uuidString.lowercased()
        var proof = InvitationProofRecord(identity: identity, installationID: profile.installationID)
        proof.phone = try InvitationWire.phone(wire); proof.secret = secret; proof.loginFingerprint = owner.fingerprint
        try store.saveProof(proof, profile: profile)
        var requests = 0, unregisters = 0
        let bridge = InvitationNotificationBridge(store: store, defaults: defaults, authorization: { owner }, registrationRequest: { requests += 1 }, unregistrationRequest: { unregisters += 1 }, permission: { true })
        bridge.activeIdentity = identity
        bridge.requestRegistration(owner: owner); bridge.requestRegistration(owner: owner)
        XCTAssertEqual(requests, 1) // same outstanding OS lifetime is idempotent.
        bridge.registrationFailed()
        bridge.requestRegistration(owner: owner); bridge.requestRegistration(owner: owner)
        XCTAssertEqual(requests, 1)
        XCTAssertTrue(bridge.registrationFailure)
        bridge.requestRegistration(owner: owner, force: true)
        XCTAssertEqual(requests, 2)
        bridge.registered(Data([1, 2])); bridge.requestRegistration(owner: owner)
        XCTAssertEqual(requests, 2)
        _ = try store.choose(false, receiverID: receiverID, name: "TV", profile: profile)
        bridge.registered(Data([3, 4])); bridge.requestRegistration(owner: owner, force: true)
        XCTAssertEqual(requests, 2)
        XCTAssertEqual(try store.profile(identity)?.choices[receiverID.uuidString.lowercased()]?.enabled, false)
        bridge.retireRegistration(unregister: true)
        XCTAssertEqual(unregisters, 1)
        XCTAssertNil(bridge.deviceToken)
    }
}

extension InvitationTests {
    func testProductionLostLocalProofListsAndDeletesSelectedHomeInstallationWithoutOtherProofs() async throws {
        let suite = "invitation-recovery-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite)); defer { defaults.removePersistentDomain(forName: suite) }
        let vault = InvitationMemoryVault(), store = InvitationSecureState(defaults: defaults, vault: { _ in vault })
        let identity = InvitationIdentity(origin: "https://home.test", instance: "instance", userID: 1)
        let owner = InvitationNotificationIdentity(identity: identity, generation: Session.shared.playbackAuthorization.generation, fingerprint: "login")
        var paths: [String] = []
        InvitationTestHTTP.answer = { request in
            paths.append(request.url!.path)
            XCTAssertNil(request.value(forHTTPHeaderField: "X-Cinema-Phone-Secret")); XCTAssertNil(request.value(forHTTPHeaderField: "X-Cinema-Grant-Secret"))
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer synthetic-home-token")
            if request.url!.path == "/api/remote/v1/phones/list" {
                return (200, [:], try self.bytes(["version": InvitationWire.version, "phones": [self.phoneWire()], "next_cursor": NSNull()]))
            }
            XCTAssertEqual(request.httpMethod, "DELETE")
            XCTAssertNil(request.httpBody); XCTAssertNil(request.httpBodyStream)
            return (200, [:], try self.bytes(["version": InvitationWire.version]))
        }
        let bridge = InvitationNotificationBridge(store: store, defaults: defaults, authorization: { owner }, registrationRequest: {}, permission: { false })
        let model = InvitationClientModel(store: store, bridge: bridge, authorization: { owner }, permission: { false }, makeAPI: { _ in self.api() })
        model.configure(active: true, remoteEnabled: false)
        XCTAssertNil(model.profile)
        defaults.set(Data("broken".utf8), forKey: "plurx.cinema-invitations.profiles.v1")
        model.configure(active: true, remoteEnabled: false)
        XCTAssertTrue(model.localStoreUnreadable)
        model.refreshHomeInstallations(); try await settle(model)
        XCTAssertEqual(model.homePhones.map(\.id), [phoneID])
        model.removeHomeInstallation(try XCTUnwrap(model.homePhones.first)); try await settle(model)
        XCTAssertTrue(model.homePhones.isEmpty)
        XCTAssertEqual(paths, ["/api/remote/v1/phones/list", "/api/remote/v1/phones/" + phoneID.uuidString.lowercased()])
    }
    func testProductionStaleAccountInstallReplyAndCorruptProfileCannotRestoreOldAuthority() async throws {
        let suite = "invitation-recovery-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite)); defer { defaults.removePersistentDomain(forName: suite) }
        let vault = InvitationMemoryVault(), store = InvitationSecureState(defaults: defaults, vault: { _ in vault })
        let identity = InvitationIdentity(origin: "https://home.test", instance: "instance", userID: 1)
        let profile = try store.create(identity, name: "Phone")
        var proof = InvitationProofRecord(identity: identity, installationID: profile.installationID)
        proof.secret = secret; try store.saveProof(proof, profile: profile)
        var owner = InvitationNotificationIdentity(identity: identity, generation: Session.shared.playbackAuthorization.generation, fingerprint: "login")
        let latch = InvitationRequestLatch(path: "/api/remote/v1/phones/list")
        InvitationTestHTTP.hold = { latch.holdFirstRequest($0) }
        InvitationTestHTTP.answer = { request in
            XCTAssertEqual(request.url!.path, "/api/remote/v1/phones/list")
            return (200, [:], try self.bytes(["version": InvitationWire.version, "phones": [self.phoneWire()], "next_cursor": NSNull()]))
        }
        let bridge = InvitationNotificationBridge(store: store, defaults: defaults, authorization: { owner }, registrationRequest: {}, permission: { false })
        let model = InvitationClientModel(store: store, bridge: bridge, authorization: { owner }, permission: { false }, makeAPI: { _ in self.api() })
        model.configure(active: false, remoteEnabled: false)
        XCTAssertEqual(model.profile?.installationID, profile.installationID)
        defaults.set(Data("broken".utf8), forKey: "plurx.cinema-invitations.profiles.v1")
        Session.shared.setCredentials(origin: "https://home.test", token: "synthetic-home-token")
        owner = .init(identity: identity, generation: Session.shared.playbackAuthorization.generation, fingerprint: "new-login")
        model.configure(active: true, remoteEnabled: false)
        XCTAssertNil(model.profile); XCTAssertNil(model.record); XCTAssertTrue(model.profiles.isEmpty)
        XCTAssertFalse(model.requiresRebind); XCTAssertFalse(model.requiresReset); XCTAssertTrue(model.localStoreUnreadable)
        model.refreshHomeInstallations()
        let deadline = ProcessInfo.processInfo.systemUptime + 5
        while !latch.isHeld && ProcessInfo.processInfo.systemUptime < deadline { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertTrue(latch.isHeld)
        let other = InvitationIdentity(origin: "https://home.test", instance: "instance", userID: 2)
        owner = .init(identity: other, generation: owner.generation + 1, fingerprint: "other-login")
        model.configure(active: true, remoteEnabled: false)
        latch.release(); try await Task.sleep(for: .milliseconds(50))
        XCTAssertTrue(model.homePhones.isEmpty); XCTAssertNil(model.profile); XCTAssertNil(model.record)
        model.resetUnreadableLocalSettings()
        XCTAssertFalse(model.localStoreUnreadable)
        XCTAssertTrue(try store.profiles().isEmpty)
        XCTAssertTrue(model.status.contains("Unknown home installations remain"))
    }
}

extension InvitationTests {
    func testProductionSessionObserverRetiresOldLoginWhileRootStaysActive() async throws {
        let suite = "invitation-auth-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite)); defer { defaults.removePersistentDomain(forName: suite) }
        let vault = InvitationMemoryVault(), store = InvitationSecureState(defaults: defaults, vault: { _ in vault })
        let identity = InvitationIdentity(origin: "https://home.test", instance: "instance", userID: 1)
        let profile = try store.create(identity, name: "Phone")
        var wire = phoneWire(); wire["installation_id"] = profile.installationID.uuidString.lowercased(); wire["permission_granted"] = false
        var proof = InvitationProofRecord(identity: identity, installationID: profile.installationID)
        proof.phone = try InvitationWire.phone(wire); proof.secret = secret
        proof.loginFingerprint = InvitationIdentity.fingerprint("synthetic-home-token"); proof.registrationAttempted = true
        try store.saveProof(proof, profile: profile)
        let auth: () -> InvitationNotificationIdentity? = {
            let session = Session.shared.playbackAuthorization
            guard let token = session.token else { return nil }
            return .init(identity: identity, generation: session.generation, fingerprint: InvitationIdentity.fingerprint(token))
        }
        var bearer: String?
        InvitationTestHTTP.answer = { request in
            bearer = request.value(forHTTPHeaderField: "Authorization")
            switch request.url!.path {
            case "/api/remote/v1/phones": return (200, [:], try self.bytes(["version": InvitationWire.version, "phone": wire, "phone_secret": NSNull()]))
            case "/api/remote/v1/invitations/consents/list": return (200, [:], try self.bytes(["version": InvitationWire.version, "consents": [], "next_cursor": NSNull()]))
            default: XCTFail("Credential rotation must not enroll or rebind automatically"); throw InvitationAPIError(code: "invalid")
            }
        }
        let bridge = InvitationNotificationBridge(store: store, defaults: defaults, authorization: auth, registrationRequest: {}, permission: { false })
        let model = InvitationClientModel(store: store, bridge: bridge, authorization: auth, permission: { false }, makeAPI: { owner in
            var api = HomeInvitationAPI(origin: identity.origin, token: Session.shared.playbackAuthorization.token!, authorizationGeneration: owner.generation)
            api.transport = self.session; return api
        })
        model.configure(active: true, remoteEnabled: false); try await settle(model)
        let previous = model.authorizationGeneration
        Session.shared.setCredentials(origin: identity.origin, token: "new-human-token")
        let deadline = ProcessInfo.processInfo.systemUptime + 5
        while (model.authorizationGeneration == previous || model.busy) && ProcessInfo.processInfo.systemUptime < deadline { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertGreaterThan(model.authorizationGeneration, previous)
        XCTAssertTrue(model.requiresRebind)
        XCTAssertEqual(bearer, "Bearer new-human-token")
        XCTAssertEqual(try store.proof(profile)?.loginFingerprint, InvitationIdentity.fingerprint("synthetic-home-token"))
    }
}
