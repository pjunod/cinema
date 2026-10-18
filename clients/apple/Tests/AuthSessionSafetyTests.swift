import Foundation
import XCTest
@testable import plurx

#if DEBUG
private final class AuthSessionHTTP: URLProtocol {
    enum Answer {
        case http(Int, String)
        case failed(URLError.Code)
        case held
    }
    private static let lock = NSLock()
    private static var answers: [String: Answer] = [:]
    private static var seen: [URLRequest] = []
    private static var held: [AuthSessionHTTP] = []
    private static var entered: (() -> Void)?
    private let stateLock = NSLock()
    private var stopped = false

    static func configure(_ values: [String: Answer], entered: (() -> Void)? = nil) {
        lock.lock(); defer { lock.unlock() }
        answers = values; seen = []; held = []; self.entered = entered
    }
    static var requests: [URLRequest] {
        lock.lock(); defer { lock.unlock() }
        return seen
    }
    static func replaceAnswers(_ values: [String: Answer], entered: (() -> Void)? = nil) {
        lock.lock(); defer { lock.unlock() }
        answers = values; self.entered = entered
    }
    static func releaseHeld(status: Int, body: String) {
        lock.lock(); let pending = held; held = []; lock.unlock()
        for request in pending { request.deliver(status: status, body: body) }
    }
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        Self.lock.lock()
        Self.seen.append(request)
        let answer = Self.answers[request.url?.path ?? ""] ?? .http(200, "{}")
        let entered = Self.entered
        if case .held = answer { Self.held.append(self) }
        Self.lock.unlock()
        switch answer {
        case .http(let status, let body): deliver(status: status, body: body)
        case .failed(let code): client?.urlProtocol(self, didFailWithError: URLError(code))
        case .held: entered?()
        }
    }
    override func stopLoading() {
        stateLock.lock(); stopped = true; stateLock.unlock()
    }
    private func deliver(status: Int, body: String) {
        stateLock.lock(); let cancelled = stopped; stateLock.unlock()
        guard !cancelled else { return }
        let response = HTTPURLResponse(url: request.url!, statusCode: status,
            httpVersion: "HTTP/1.1", headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(body.utf8))
        client?.urlProtocolDidFinishLoading(self)
    }
}

@MainActor
final class AuthSessionSafetyTests: XCTestCase {
    private actor LogoutGate {
        private var continuation: CheckedContinuation<Void, Never>?
        private var finished = false
        func wait() async {
            guard !finished else { return }
            await withCheckedContinuation { continuation = $0 }
        }
        func finish() { finished = true; continuation?.resume(); continuation = nil }
    }
    private final class Vault: TokenStoring {
        var value: String? = "saved-bearer"
        func read() -> String? { value }
        func write(_ token: String) -> Bool { value = token; return true }
        func clear() { value = nil }
    }
    private let server = #"{"instance_id":"saved-instance","name":"Test Cinema"}"#

    private func withModel(findSavedServers: @escaping () async -> [DiscoveredServer] = { [] },
                           logoutRequest: ((String, String) async throws -> Void)? = nil,
                           _ body: (AppModel, SettingsStore, Vault, UserDefaults, String) async throws -> Void) async throws {
        let name = "plurx-auth-safety-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: name))
        let arguments = defaults.volatileDomain(forName: UserDefaults.argumentDomain)
        let prior = Session.shared.credentials
        let vault = Vault()
        let settings = SettingsStore(defaults: defaults, tokenVault: vault, persistentDomainName: name)
        settings.setServer(origin: "https://saved.invalid", instanceId: "saved-instance", token: "saved-bearer")
        settings.username = "saved-viewer"
        settings.userId = 1
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [AuthSessionHTTP.self]
        let transport = URLSession(configuration: config)
        defer {
            defaults.setVolatileDomain(arguments, forName: UserDefaults.argumentDomain)
            defaults.removePersistentDomain(forName: name)
            transport.invalidateAndCancel()
            Session.shared.setCredentials(origin: prior.origin, token: prior.token)
        }
        let model = AppModel(startServices: false, settings: settings, makeAPI: { origin in
            var api = PlurxAPI(origin: origin); api.testTransport = transport; return api
        }, findSavedServers: findSavedServers, logoutRequest: logoutRequest)
        defer { model.discovery.stop() }
        try await body(model, settings, vault, defaults, name)
    }

    func testLaunchArgumentsCannotRePairSavedOriginIdentityOrLegacyBearer() async throws {
        try await withModel { _, settings, vault, defaults, _ in
            vault.value = nil
            defaults.set("legacy-saved-bearer", forKey: "plurx.token")
            defaults.setVolatileDomain(["plurx.origin": "https://override.invalid",
                "plurx.instanceId": "other-instance", "plurx.token": "argument-bearer"],
                forName: UserDefaults.argumentDomain)
            XCTAssertEqual(defaults.string(forKey: "plurx.origin"), "https://override.invalid",
                "Reproduce Foundation's merged launch-argument override")
            XCTAssertEqual(settings.origin, "https://saved.invalid")
            XCTAssertEqual(settings.instanceId, "saved-instance")
            XCTAssertEqual(settings.token, "legacy-saved-bearer")
            XCTAssertEqual(vault.value, "legacy-saved-bearer")
        }
    }

    func testAnonymousBootstrap401And403DoNotClearOrRedirectSavedSession() async throws {
        for status in [401, 403] {
            AuthSessionHTTP.configure(["/api/v1/server": .http(status, "{}")])
            try await withModel { model, settings, vault, defaults, _ in
                defaults.setVolatileDomain(["plurx.origin": "https://override.invalid"],
                    forName: UserDefaults.argumentDomain)
                await model.retrySavedSession()
                XCTAssertEqual(model.phase, .reconnectFailed)
                XCTAssertEqual(settings.origin, "https://saved.invalid")
                XCTAssertEqual(settings.instanceId, "saved-instance")
                XCTAssertEqual(vault.value, "saved-bearer")
                XCTAssertFalse(AuthSessionHTTP.requests.isEmpty)
                XCTAssertTrue(AuthSessionHTTP.requests.allSatisfy {
                    $0.url?.host == "saved.invalid" && $0.value(forHTTPHeaderField: "Authorization") == nil
                })
            }
        }
    }

    func testBootstrapTimeoutPreservesSavedOriginIdentityAndToken() async throws {
        AuthSessionHTTP.configure(["/api/v1/server": .failed(.timedOut)])
        try await withModel { model, settings, vault, _, _ in
            await model.retrySavedSession()
            XCTAssertEqual(model.phase, .reconnectFailed)
            XCTAssertEqual(settings.origin, "https://saved.invalid")
            XCTAssertEqual(settings.instanceId, "saved-instance")
            XCTAssertEqual(vault.value, "saved-bearer")
            XCTAssertTrue(AuthSessionHTTP.requests.allSatisfy { $0.value(forHTTPHeaderField: "Authorization") == nil })
        }
    }

    func testWrongInstanceNeverReceivesTheSavedBearer() async throws {
        AuthSessionHTTP.configure(["/api/v1/server": .http(200, #"{"instance_id":"other-instance"}"#)])
        try await withModel { model, settings, vault, _, _ in
            await model.retrySavedSession()
            XCTAssertEqual(model.phase, .reconnectFailed)
            XCTAssertEqual(settings.origin, "https://saved.invalid")
            XCTAssertEqual(settings.instanceId, "saved-instance")
            XCTAssertEqual(vault.value, "saved-bearer")
            XCTAssertEqual(AuthSessionHTTP.requests.map { $0.url?.path }, ["/api/v1/server"])
            XCTAssertNil(AuthSessionHTTP.requests.first?.value(forHTTPHeaderField: "Authorization"))
            XCTAssertNil(Session.shared.credentials.token)
        }
    }

    func testAuthenticatedBootstrapTimeoutDoesNotDiscardSavedPairing() async throws {
        AuthSessionHTTP.configure(["/api/v1/server": .http(200, server), "/api/v1/me": .failed(.timedOut)])
        try await withModel { model, settings, vault, _, _ in
            await model.retrySavedSession()
            XCTAssertEqual(model.phase, .reconnectFailed)
            XCTAssertEqual(settings.origin, "https://saved.invalid")
            XCTAssertEqual(settings.instanceId, "saved-instance")
            XCTAssertEqual(vault.value, "saved-bearer")
            XCTAssertEqual(AuthSessionHTTP.requests.last?.value(forHTTPHeaderField: "Authorization"), "Bearer saved-bearer")
        }
    }

    func testVerifiedSavedServerCanRejectAnExpiredToken() async throws {
        for status in [401, 403] {
            AuthSessionHTTP.configure(["/api/v1/server": .http(200, server), "/api/v1/me": .http(status, "{}")])
            try await withModel { model, settings, vault, _, _ in
                await model.retrySavedSession()
                XCTAssertEqual(model.phase, .needLogin)
                XCTAssertNil(vault.value)
                XCTAssertEqual(settings.origin, "https://saved.invalid")
                XCTAssertEqual(AuthSessionHTTP.requests.last?.value(forHTTPHeaderField: "Authorization"), "Bearer saved-bearer")
            }
        }
    }

    func testChangeServerWinsAgainstLateBootstrapResponse() async throws {
        let entered = expectation(description: "identity request pending")
        AuthSessionHTTP.configure(["/api/v1/server": .held], entered: { entered.fulfill() })
        try await withModel { model, settings, vault, _, _ in
            let pending = Task { await model.retrySavedSession() }
            await fulfillment(of: [entered], timeout: 3)
            model.changeServer()
            AuthSessionHTTP.releaseHeld(status: 200, body: server)
            await pending.value
            XCTAssertEqual(model.phase, .needServer)
            XCTAssertFalse(model.busy)
            XCTAssertEqual(settings.origin, "")
            XCTAssertNil(vault.value)
            XCTAssertNil(Session.shared.credentials.token)
            XCTAssertEqual(AuthSessionHTTP.requests.count, 1, "The superseded bootstrap must not send /me")
        }
    }

    func testChangeServerWinsAgainstLateLoginSuccess() async throws {
        let entered = expectation(description: "login request pending")
        AuthSessionHTTP.configure(["/api/v1/server": .http(200, server), "/api/v1/auth/login": .held],
            entered: { entered.fulfill() })
        try await withModel { model, settings, vault, _, _ in
            await model.connect("https://saved.invalid")
            let pending = Task { await model.login("viewer", "disposable") }
            await fulfillment(of: [entered], timeout: 3)
            model.changeServer()
            AuthSessionHTTP.releaseHeld(status: 200, body: #"{"token":"late-bearer","user":{"id":7,"username":"viewer"}}"#)
            await pending.value
            XCTAssertEqual(model.phase, .needServer)
            XCTAssertFalse(model.busy)
            XCTAssertEqual(settings.origin, "")
            XCTAssertNil(vault.value)
            XCTAssertNil(Session.shared.credentials.token)
        }
    }

    func testChangeServerWinsAgainstLateConnectSuccess() async throws {
        let entered = expectation(description: "candidate identity request pending")
        AuthSessionHTTP.configure(["/api/v1/server": .held], entered: { entered.fulfill() })
        try await withModel { model, settings, vault, _, _ in
            let pending = Task { await model.connect("https://candidate.invalid") }
            await fulfillment(of: [entered], timeout: 3)
            model.changeServer()
            AuthSessionHTTP.releaseHeld(status: 200, body: server)
            await pending.value
            XCTAssertEqual(model.phase, .needServer)
            XCTAssertFalse(model.busy)
            XCTAssertEqual(settings.origin, "")
            XCTAssertNil(vault.value)
            XCTAssertNil(Session.shared.credentials.token)
        }
    }

    func testOldHome401CannotSignOutANewLoginOrBlockItsHomeLoad() async throws {
        let entered = expectation(description: "old home request pending")
        AuthSessionHTTP.configure(["/api/v1/server": .http(200, server),
            "/api/v1/me": .http(200, #"{"id":1,"username":"saved-viewer"}"#),
            "/api/v1/hubs": .held, "/api/v1/libraries": .http(200, "[]")],
            entered: { entered.fulfill() })
        try await withModel { model, settings, vault, _, _ in
            let oldBootstrap = Task { await model.retrySavedSession() }
            await fulfillment(of: [entered], timeout: 3)
            model.changeServer()
            AuthSessionHTTP.replaceAnswers(["/api/v1/server": .http(200, server),
                "/api/v1/auth/login": .http(200, #"{"token":"new-bearer","user":{"id":7,"username":"new-viewer"}}"#),
                "/api/v1/hubs": .http(200, "{}"), "/api/v1/libraries": .http(200, "[]")])
            await model.connect("https://new.invalid")
            let newHomeCompleted = expectation(description: "new login and home finish independently")
            let newLogin = Task {
                await model.login("new-viewer", "disposable")
                newHomeCompleted.fulfill()
            }
            // Returning from the new Home request before the old response is
            // released proves it did not coalesce behind the old task.
            await fulfillment(of: [newHomeCompleted], timeout: 3)
            XCTAssertEqual(model.phase, .ready)
            XCTAssertFalse(model.homeLoading)
            AuthSessionHTTP.releaseHeld(status: 401, body: "{}")
            await oldBootstrap.value
            await newLogin.value
            XCTAssertEqual(model.phase, .ready)
            XCTAssertEqual(settings.origin, "https://new.invalid")
            XCTAssertEqual(vault.value, "new-bearer")
            XCTAssertEqual(Session.shared.credentials.token, "new-bearer")
            XCTAssertNil(model.homeError)
            XCTAssertTrue(AuthSessionHTTP.requests.contains { $0.url?.host == "new.invalid" && $0.url?.path == "/api/v1/hubs" })
        }
    }

    func testTemporaryAcceptanceHome401PreservesSavedAccount() async throws {
        AuthSessionHTTP.configure(["/api/v1/server": .http(200, server),
            "/api/v1/me": .http(200, #"{"id":7,"username":"temporary-viewer"}"#),
            "/api/v1/hubs": .http(401, "{}"), "/api/v1/libraries": .http(200, "[]")])
        try await withModel { model, settings, vault, defaults, _ in
            defaults.setVolatileDomain(["plurx.token": "temporary-bearer"], forName: UserDefaults.argumentDomain)
            model.testAcceptanceLaunch = PlaybackAcceptanceLaunch(requestedOrigin: "https://temporary.invalid",
                itemId: 1, fileId: 2, startMs: 0, durationMs: 1_000, title: "Disposable", height: nil, probesEnabled: false)
            await model.retrySavedSession()
            XCTAssertEqual(model.phase, .reconnectFailed)
            XCTAssertEqual(settings.origin, "https://saved.invalid")
            XCTAssertEqual(settings.instanceId, "saved-instance")
            XCTAssertEqual(settings.username, "saved-viewer")
            XCTAssertEqual(settings.userId, 1)
            XCTAssertEqual(vault.value, "saved-bearer")
            XCTAssertNil(Session.shared.credentials.token)
        }
    }

    func testOldLogoutCannotClearANewerConnectBusyState() async throws {
        let entered = expectation(description: "logout pending")
        let gate = LogoutGate()
        try await withModel(logoutRequest: { _, _ in entered.fulfill(); await gate.wait() }) { model, _, _, _, _ in
            Session.shared.setCredentials(origin: "https://saved.invalid", token: "saved-bearer")
            model.logout()
            await fulfillment(of: [entered], timeout: 3)
            model.changeServer()
            let connecting = expectation(description: "new connect pending")
            AuthSessionHTTP.configure(["/api/v1/server": .held], entered: { connecting.fulfill() })
            let newConnect = Task { await model.connect("https://new.invalid") }
            await fulfillment(of: [connecting], timeout: 3)
            XCTAssertTrue(model.busy)
            await gate.finish()
            await model.waitForLogoutForTesting()
            XCTAssertTrue(model.busy, "The stale logout must leave the new connect's spinner alone")
            AuthSessionHTTP.releaseHeld(status: 200, body: server)
            await newConnect.value
            XCTAssertEqual(model.phase, .needLogin)
            XCTAssertFalse(model.busy)
        }
    }

    func testRecoveryActionsStayAvailableDuringDiscoveryAndChangeServerWins() async throws {
        let entered = expectation(description: "saved-server discovery pending")
        let gate = LogoutGate()
        AuthSessionHTTP.configure(["/api/v1/server": .failed(.timedOut)])
        try await withModel(findSavedServers: {
            entered.fulfill()
            await gate.wait()
            return []
        }) { model, _, vault, _, _ in
            let pending = Task { await model.retrySavedSession() }
            await fulfillment(of: [entered], timeout: 3)
            XCTAssertEqual(model.phase, .reconnectFailed,
                "ReconnectView must expose retry and server choice before discovery finishes")
            XCTAssertTrue(model.authError?.contains("Searching this network") == true)
            XCTAssertEqual(vault.value, "saved-bearer")
            model.changeServer()
            await gate.finish()
            await pending.value
            XCTAssertEqual(model.phase, .needServer)
            XCTAssertNil(model.authError)
        }
    }
}
#endif
