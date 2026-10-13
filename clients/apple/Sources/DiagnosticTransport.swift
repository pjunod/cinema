import Foundation

/// Cookies, caches, redirects and Cinema authorization are absent from the
/// support session. Every operation rechecks the original authorization epoch.
final class DiagnosticTransport: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    enum Failure: Error, LocalizedError {
        case invalid, limit, retired, storage, consent, unavailable, http(Int)
        var errorDescription: String? {
            switch self {
            case .storage: return "Protected report storage is unavailable. Keep this screen open or save a local file."
            case .consent: return "Confirm the support destination before uploading."
            case .retired: return "The account or report operation changed. Reopen this account's report."
            case .http(let status): return "Support refused the request (HTTP \(status)). The local report remains available."
            case .limit: return "The report or reply exceeds its size limit."
            case .unavailable: return "Support is unavailable. Retry or save the local report."
            case .invalid: return "The report or support response is invalid."
            }
        }
    }
    struct Offer: Codable { let id: String; let manual_code: String; let url: String; let expires_in_ms: UInt64 }
    private struct InvitationAuthority: Codable { let origin: String; let secret: String }
    struct Secrets: Codable { var upload: String?; var management: String? }
    struct Enrollment: Codable { let origin: String; let authorization: String; let expires_in_ms: UInt64; var expires: UInt64? }
    private struct EnrollmentReply: Decodable { let origin: String; let authorization: Credential; struct Credential: Decodable { let enrollment: String; let expires_in_ms: UInt64 } }
    struct Grant: Decodable { let upload_grant: String; let expires_in_ms: UInt64 }
    struct Reservation: Decodable { let upload_id: String; let management_grant: String; let chunk_bytes: Int }
    struct Status: Decodable { let state: String; let received: [Int]?; let reference: String? }
    struct HandoffStatus: Decodable { let state: String; let comparison: String?; let reference: String? }
    private struct GrantReply: Decodable { let origin: String; let grant: Grant }
    private struct Capabilities: Decodable { let support: Support; struct Support: Decodable { let configured: Bool; let origin: String } }
    struct Projection: Decodable { let version: String; let state: String; let events: [DiagnosticEvent]?; let components: [DiagnosticReport.Component]?; let dropped_events: String? }
    private struct Created: Decodable { let id: String }
    private let metricsLock = NSLock()
    private final class CheckProgress: @unchecked Sendable {
        private let lock = NSLock()
        private var events: [DiagnosticEvent] = []
        func append(_ value: DiagnosticEvent) { lock.withLock { if events.count < 100 { events.append(value) } } }
        func snapshot() -> [DiagnosticEvent] { lock.withLock { events } }
    }
    private var collectingMetrics = false
    private var phaseMetrics: [(String, UInt64)] = []
    let scope: String
    let authorization: (origin: String, token: String?, generation: UInt64)
    private lazy var session: URLSession = {
        let config = URLSessionConfiguration.ephemeral; config.httpCookieStorage = nil; config.httpShouldSetCookies = false
        config.urlCache = nil; config.requestCachePolicy = .reloadIgnoringLocalCacheData; config.waitsForConnectivity = false
        config.timeoutIntervalForRequest = 10; config.timeoutIntervalForResource = 10
        return URLSession(configuration: config, delegate: self, delegateQueue: nil)
    }()
    init(scope: String, authorization: (origin: String, token: String?, generation: UInt64) = Session.shared.playbackAuthorization) { self.scope = scope; self.authorization = authorization }
    func cancel() { session.invalidateAndCancel() }
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) { completionHandler(nil) }
    func urlSession(_ session: URLSession, task: URLSessionTask, didFinishCollecting metrics: URLSessionTaskMetrics) {
        metricsLock.lock(); defer { metricsLock.unlock() }
        guard collectingMetrics, Session.shared.playbackAuthorization.generation == authorization.generation else { return }
        for transaction in metrics.transactionMetrics.prefix(4) {
            for (phase, start, end) in [("dns", transaction.domainLookupStartDate, transaction.domainLookupEndDate), ("connect", transaction.connectStartDate, transaction.connectEndDate), ("tls", transaction.secureConnectionStartDate, transaction.secureConnectionEndDate)] {
                if let start, let end, end >= start, phaseMetrics.count < 16 { phaseMetrics.append((phase, UInt64(min(15_000, end.timeIntervalSince(start) * 1_000)))) }
            }
        }
    }
    func current() throws {
        try Task.checkCancellation()
        guard Session.shared.playbackAuthorization.generation == authorization.generation else { throw Failure.retired }
    }
    static func origin(_ text: String) throws -> String {
        guard let url = URLComponents(string: text), url.user == nil, url.password == nil, url.query == nil, url.fragment == nil,
              url.path.isEmpty || url.path == "/", url.host != nil,
              url.scheme == "https" || (url.scheme == "http" && ["localhost","127.0.0.1","::1"].contains(url.host!) && UserDefaults.standard.bool(forKey: "plurx.diagnosticDevelopmentLocal")),
              let value = Session.canonicalOrigin(text) else { throw Failure.invalid }
        return value
    }
    private static func vault(_ id: String, scope: String) -> TokenVault { TokenVault(service: "tv.plurx.diagnostics", account: scope + ":" + id) }
    static func clearSecrets(_ id: String, scope: String) {
        vault(id, scope: scope).clear()
        if UUID(uuidString: id)?.uuidString.lowercased() == id { vault(id + ":invitation", scope: scope).clear(); vault(id + ":invitation-proof", scope: scope).clear() }
    }
    private func secrets(_ id: String) -> Secrets {
        guard let value = Self.vault(id, scope: scope).read()?.data(using: .utf8), value.count <= 1024,
              let secrets = try? JSONDecoder().decode(Secrets.self, from: value) else { return Secrets() }
        return secrets
    }
    private func save(_ secrets: Secrets, id: String) throws {
        try current()
        guard let text = String(data: try JSONEncoder().encode(secrets), encoding: .utf8), Self.vault(id, scope: scope).write(text) else { throw Failure.storage }
    }
    private func request(_ origin: String, path: String, method: String = "POST", body: Data? = nil, grant: String? = nil, cinema: Bool = false, chunkHash: String? = nil, limit: Int = 262_144, validateJSON: Bool = true, retiredCleanup: Bool = false) async throws -> Data {
        let cleanup = retiredCleanup && cinema && origin == authorization.origin && method == "DELETE" && body == nil && grant == nil
            && DiagnosticWire.matches(path, "^/api/v1/diagnostics/snapshots/[a-f0-9-]{36}$")
        if !cleanup { try current() }
        let base = cinema ? (Session.canonicalOrigin(origin) ?? "") : try Self.origin(origin)
        guard !base.isEmpty, path.hasPrefix("/"), !path.hasPrefix("//"), let url = URL(string: base + path) else { throw Failure.invalid }
        var req = URLRequest(url: url); req.httpMethod = method; req.httpBody = body
        if let body { req.setValue(chunkHash == nil ? "application/json" : "application/octet-stream", forHTTPHeaderField: "Content-Type"); guard body.count <= 1_048_576 else { throw Failure.limit } }
        if let grant { guard DiagnosticWire.matches(grant, "^[a-f0-9]{64}$") else { throw Failure.invalid }; req.setValue("Bearer " + grant, forHTTPHeaderField: "Authorization") }
        else if cinema, let token = authorization.token { req.setValue("Bearer " + token, forHTTPHeaderField: "Authorization") }
        if let chunkHash { req.setValue(chunkHash, forHTTPHeaderField: "X-Chunk-Sha256") }
        let (stream, response) = try await session.bytes(for: req); if !cleanup { try current() }
        guard let response = response as? HTTPURLResponse, response.url?.absoluteString == url.absoluteString else { throw Failure.invalid }
        guard response.expectedContentLength <= limit || response.expectedContentLength < 0 else { throw Failure.limit }
        var bytes = Data(); bytes.reserveCapacity(min(limit, 16_384))
        for try await byte in stream { bytes.append(byte); if bytes.count > limit { throw Failure.limit }; if !cleanup && bytes.count % 4096 == 0 { try current() } }
        if !cleanup { try current() }; guard (200..<300).contains(response.statusCode) else { throw Failure.http(response.statusCode) }
        if validateJSON && !bytes.isEmpty { try RemoteStrictJSON.check(bytes) }; return bytes
    }
    func connectionChecks() async throws -> [DiagnosticEvent] {
        try current(); let progress = CheckProgress(); metricsLock.withLock { phaseMetrics = []; collectingMetrics = true }
        defer { metricsLock.withLock { collectingMetrics = false; phaseMetrics = [] } }
        return try await withThrowingTaskGroup(of: [DiagnosticEvent].self) { group in
            group.addTask { [self] in
                let run = UUID().uuidString.lowercased(); let started = ProcessInfo.processInfo.systemUptime; var sequence: UInt64 = 0
                var events: [DiagnosticEvent] = []; var bytes = 0
                func event(_ facts: [String: DiagnosticFact]) {
                    sequence += 1
                    events.append(DiagnosticEvent(run_id: run, sequence: String(sequence), utc: DiagnosticRecorder.utc(),
                        elapsed_ms: UInt64(min(15_000, max(0, (ProcessInfo.processInfo.systemUptime - started) * 1_000))), domain: "network", kind: "request", facts: facts))
                    if let last = events.last { progress.append(last) }
                }
                let paths = [("/healthz", "connect"), ("/readyz", "readiness"), ("/api/v1/server", "readiness")] + (authorization.token == nil ? [] : [("/api/v1/me", "auth")])
                for (path, phase) in paths {
                    try Task.checkCancellation(); try current(); let before = ProcessInfo.processInfo.systemUptime
                    var facts: [String: DiagnosticFact] = ["phase": .text(phase), "method": .text("GET"), "route": .text(phase == "auth" ? "auth" : "other")]
                    do {
                        let data = try await request(authorization.origin, path: path, method: "GET", cinema: true, limit: max(1, 2_097_152 - bytes), validateJSON: false)
                        bytes += data.count; facts["observed"] = .flag(true)
                    } catch {
                        try Task.checkCancellation(); try current(); facts["observed"] = .flag(false)
                        if case Failure.http(let status) = error { facts["status"] = .number(UInt64(status)); facts["failure"] = .text(status == 401 || status == 403 ? "authentication" : "http") }
                        else if let url = error as? URLError {
                            facts["failure"] = .text([.cannotFindHost, .dnsLookupFailed].contains(url.code) ? "dns" : [.secureConnectionFailed, .serverCertificateUntrusted, .serverCertificateHasBadDate, .serverCertificateNotYetValid, .serverCertificateHasUnknownRoot].contains(url.code) ? "tls" : url.code == .cannotConnectToHost ? "connect" : "network")
                        } else { facts["failure"] = .text("network") }
                    }
                    facts["duration_ms"] = .number(UInt64(min(15_000, max(0, (ProcessInfo.processInfo.systemUptime - before) * 1_000)))); event(facts)
                    if bytes >= 2_097_152 { break }
                }
                if bytes < 2_097_152 {
                    let wall = Date().timeIntervalSince1970 * 1_000; let before = ProcessInfo.processInfo.systemUptime
                    if let data = try? await request(authorization.origin, path: "/api/v1/diagnostics/capabilities", method: "GET", cinema: true, limit: 2_097_152 - bytes),
                       let reply = try? JSONSerialization.jsonObject(with: data) as? [String: Any], let server = reply["server_time_ms"] as? NSNumber {
                        let duration = (ProcessInfo.processInfo.systemUptime - before) * 1_000; let offset = server.doubleValue - wall - duration / 2
                        if offset.isFinite && abs(offset) <= 604_800_000 && duration <= 15_000 {
                            event(["observed": .flag(true), "clock_ahead_ms": .number(UInt64(max(0, offset))), "clock_behind_ms": .number(UInt64(max(0, -offset))), "clock_uncertainty_ms": .number(UInt64(max(0, duration)) + 1)])
                        }
                    }
                }
                let phases = metricsLock.withLock { phaseMetrics }
                for (phase, duration) in phases { event(["phase": .text(phase), "duration_ms": .number(duration), "observed": .flag(true)]) }
                for phase in ["dns","connect","tls"] where !phases.contains(where: { $0.0 == phase }) { event(["phase": .text(phase), "observed": .flag(false), "reason": .text("context_unavailable")]) }
                event(["phase": .text("delivery"), "observed": .flag(false), "reason": .text("not_requested")])
                return events
            }
            group.addTask { [self] in
                try await Task.sleep(for: .seconds(15)); try current()
                var partial = progress.snapshot()
                partial.append(DiagnosticEvent(run_id: UUID().uuidString.lowercased(), sequence: "1", utc: DiagnosticRecorder.utc(), elapsed_ms: 15_000,
                    domain: "network", kind: "availability", facts: ["phase": .text("unavailable"), "observed": .flag(false), "reason": .text("timeout")]))
                return partial
            }
            defer { group.cancelAll() }; guard let events = try await group.next() else { throw Failure.unavailable }; return events
        }
    }
    private func decoded<T: Decodable>(_ type: T.Type, _ origin: String, path: String, method: String = "POST", body: Data? = nil, grant: String? = nil, cinema: Bool = false, chunkHash: String? = nil, limit: Int = 262_144) async throws -> T {
        try JSONDecoder().decode(type, from: await request(origin, path: path, method: method, body: body, grant: grant, cinema: cinema, chunkHash: chunkHash, limit: limit))
    }
    func supportOrigin() async throws -> String {
        if let enrollment = enrollment() { return try Self.origin(enrollment.origin) }
        let reply = try await decoded(Capabilities.self, authorization.origin, path: "/api/v1/diagnostics/capabilities", method: "GET", cinema: true)
        guard reply.support.configured else { throw Failure.unavailable }; return try Self.origin(reply.support.origin)
    }
    private var installation: String {
        let key = "plurx.diagnosticInstallation"; if let id = UserDefaults.standard.string(forKey: key), UUID(uuidString: id)?.uuidString.lowercased() == id { return id }
        let id = UUID().uuidString.lowercased(); UserDefaults.standard.set(id, forKey: key); return id
    }
    private func enrollment() -> Enrollment? {
        guard scope != "anonymous", let value = Self.vault("enrollment", scope: scope).read()?.data(using: .utf8), value.count <= 2048,
              let e = try? JSONDecoder().decode(Enrollment.self, from: value), (e.expires ?? 0) > UInt64(Date().timeIntervalSince1970 * 1000),
              DiagnosticWire.matches(e.authorization, "^[a-f0-9]{64}$") else { return nil }; return e
    }
    func provision() async throws {
        guard scope != "anonymous", authorization.token != nil else { return }; if enrollment() != nil { return }
        let reply = try await decoded(EnrollmentReply.self, authorization.origin, path: "/api/v1/diagnostics/enrollments", body: JSONSerialization.data(withJSONObject: ["installation_id": installation]), cinema: true)
        var value = Enrollment(origin: reply.origin, authorization: reply.authorization.enrollment, expires_in_ms: reply.authorization.expires_in_ms)
        _ = try Self.origin(value.origin); guard DiagnosticWire.matches(value.authorization, "^[a-f0-9]{64}$"), value.expires_in_ms <= 86_400_000 else { throw Failure.invalid }
        value.expires = UInt64(Date().timeIntervalSince1970 * 1_000) + value.expires_in_ms
        try current(); guard let text = String(data: try JSONEncoder().encode(value), encoding: .utf8), Self.vault("enrollment", scope: scope).write(text) else { throw Failure.storage }
    }
    func revokeEnrollment() async {
        _ = try? await request(authorization.origin, path: "/api/v1/diagnostics/enrollments", method: "DELETE", body: JSONSerialization.data(withJSONObject: ["installation_id": installation]), cinema: true)
        if (try? current()) != nil { Self.clearSecrets("enrollment", scope: scope) }
    }
    func invitation(origin: String, code: String, reportID: String) async throws {
        guard DiagnosticWire.matches(code, "^[A-Z2-9]{10}$"), UUID(uuidString: reportID)?.uuidString.lowercased() == reportID else { throw Failure.invalid }
        let id = reportID + ":invitation-proof"
        let proof: String
        if let prior = Self.vault(id, scope: scope).read(), DiagnosticWire.matches(prior, "^[a-f0-9]{64}$") { proof = prior }
        else {
            let bytes = Data((0..<32).map { _ in UInt8.random(in: .min ... .max) }); proof = bytes.map { String(format: "%02x", $0) }.joined()
            guard Self.vault(id, scope: scope).write(proof) else { throw Failure.storage }
        }
        struct Reply: Decodable { let invitation: String; let expires_in_ms: UInt64 }
        let value = try await decoded(Reply.self, origin, path: "/support/v1/invitations/redeem", body: JSONSerialization.data(withJSONObject: ["code": code, "proof": proof]))
        guard DiagnosticWire.matches(value.invitation, "^[a-f0-9]{64}$"), value.expires_in_ms <= 86_400_000 else { throw Failure.invalid }
        try current(); guard let saved = String(data: try JSONEncoder().encode(InvitationAuthority(origin: try Self.origin(origin), secret: value.invitation)), encoding: .utf8), Self.vault(reportID + ":invitation", scope: scope).write(saved) else { throw Failure.storage }
    }
    func snapshot(_ capture: DiagnosticCapture, includeSystem: Bool = false) async throws -> Projection {
        guard authorization.token != nil, capture.namespace != "anonymous" else { throw Failure.unavailable }
        var fields: [String: Any] = ["incident_at_ms": capture.incidentMs, "window_ms": 600_000, "include_system": includeSystem]
        if let event = capture.events.last(where: { $0.facts["correlation"] != nil }), case .text(let correlation) = event.facts["correlation"] { fields["correlation"] = correlation }
        let body = try JSONSerialization.data(withJSONObject: fields)
        let created = try await decoded(Created.self, authorization.origin, path: "/api/v1/diagnostics/snapshots", body: body, cinema: true)
        guard UUID(uuidString: created.id)?.uuidString.lowercased() == created.id else { throw Failure.invalid }
        let deadline = ContinuousClock.now.advanced(by: .seconds(6))
        do {
            while ContinuousClock.now < deadline {
                let value = try await decoded(Projection.self, authorization.origin, path: "/api/v1/diagnostics/snapshots/" + created.id, method: "GET", cinema: true, limit: 2_097_152)
                guard value.version == DiagnosticWire.version else { throw Failure.invalid }
                if value.state != "pending" {
                    guard (value.events?.count ?? 0) <= 10_000, value.events?.allSatisfy(\.valid) == true, value.components?.allSatisfy(\.valid) == true else { throw Failure.invalid }
                    _ = try? await request(authorization.origin, path: "/api/v1/diagnostics/snapshots/" + created.id, method: "DELETE", cinema: true)
                    return value
                }
                try await Task.sleep(for: .milliseconds(250))
            }
            throw Failure.unavailable
        } catch {
            // A captured cleanup transport cannot borrow another account.
            let cleanup = DiagnosticTransport(scope: scope, authorization: authorization)
            Task { _ = try? await cleanup.request(cleanup.authorization.origin, path: "/api/v1/diagnostics/snapshots/" + created.id, method: "DELETE", cinema: true, retiredCleanup: true); cleanup.cancel() }
            throw error
        }
    }
    func stage(_ archive: DiagnosticReport.Archive, draft: inout DiagnosticStore.Draft) async throws {
        guard draft.consented, draft.id == archive.binding.report_id, draft.capture.namespace == scope else { throw Failure.consent }
        guard let destination = draft.origin else { throw Failure.invalid }; let origin = try Self.origin(destination)
        var authority = secrets(draft.id)
        if authority.upload == nil {
            let grant: Grant
            if let data = Self.vault(draft.id + ":invitation", scope: scope).read()?.data(using: .utf8), let invitation = try? JSONDecoder().decode(InvitationAuthority.self, from: data) {
                guard invitation.origin == origin, DiagnosticWire.matches(invitation.secret, "^[a-f0-9]{64}$") else { throw Failure.invalid }
                grant = try await decoded(Grant.self, origin, path: "/support/v1/grants", body: JSONEncoder().encode(archive.binding), grant: invitation.secret)
            } else if let enrollment = enrollment(), try Self.origin(enrollment.origin) == origin {
                grant = try await decoded(Grant.self, origin, path: "/support/v1/grants", body: JSONEncoder().encode(archive.binding), grant: enrollment.authorization)
            } else {
                let reply = try await decoded(GrantReply.self, authorization.origin, path: "/api/v1/diagnostics/upload-grants", body: JSONEncoder().encode(archive.binding), cinema: true)
                guard try Self.origin(reply.origin) == origin else { throw Failure.invalid }; grant = reply.grant
            }
            guard DiagnosticWire.matches(grant.upload_grant, "^[a-f0-9]{64}$"), grant.expires_in_ms <= 1_800_000 else { throw Failure.invalid }
            authority.upload = grant.upload_grant; try save(authority, id: draft.id)
            draft.grantExpires = UInt64(Date().timeIntervalSince1970 * 1_000) + grant.expires_in_ms; await DiagnosticStore.shared.save(draft)
        }
        if authority.management == nil {
            let reply = try await decoded(Reservation.self, origin, path: "/support/v1/uploads", body: JSONEncoder().encode(archive.binding), grant: authority.upload)
            guard reply.upload_id == draft.id, reply.chunk_bytes == 1_048_576, DiagnosticWire.matches(reply.management_grant, "^[a-f0-9]{64}$") else { throw Failure.invalid }
            authority.management = reply.management_grant; try save(authority, id: draft.id)
        }
        let status = try await decoded(Status.self, origin, path: "/support/v1/uploads/" + draft.id + "/status", method: "GET", grant: authority.management)
        if ["staged","submitted"].contains(status.state) { draft.state = status.state; draft.reference = status.reference; await DiagnosticStore.shared.save(draft); return }
        // Creator refresh is bound to this immutable capture, also after skew
        // or suspension. It never obtains authority over somebody else's report.
        let refresh = try await decoded(Grant.self, origin, path: "/support/v1/uploads/" + draft.id + "/refresh", grant: authority.management)
        guard DiagnosticWire.matches(refresh.upload_grant, "^[a-f0-9]{64}$") else { throw Failure.invalid }
        authority.upload = refresh.upload_grant; try save(authority, id: draft.id)
        let count = (archive.bytes.count + 1_048_575) / 1_048_576; let received = Set(status.received ?? [])
        guard received.allSatisfy({ $0 >= 0 && $0 < count }) else { throw Failure.invalid }
        for index in 0..<count where !received.contains(index) {
            try current(); let bytes = archive.bytes.subdata(in: (index * 1_048_576)..<min(archive.bytes.count, (index + 1) * 1_048_576))
            _ = try await request(origin, path: "/support/v1/uploads/" + draft.id + "/chunks/" + String(index), method: "PUT", body: bytes, grant: authority.upload, chunkHash: DiagnosticRecorder.digest(bytes))
            draft.state = "uploading"; await DiagnosticStore.shared.save(draft)
        }
        let complete = try await decoded(Status.self, origin, path: "/support/v1/uploads/" + draft.id + "/complete", grant: authority.upload)
        guard complete.state == "staged" || complete.state == "submitted" else { throw Failure.invalid }
        draft.state = complete.state; draft.reference = complete.reference; await DiagnosticStore.shared.save(draft)
    }
    func submit(_ draft: inout DiagnosticStore.Draft) async throws {
        guard let origin = draft.origin, let management = secrets(draft.id).management, draft.consented else { throw Failure.consent }
        let value = try await decoded(Status.self, origin, path: "/support/v1/reports/" + draft.id + "/submit", body: JSONSerialization.data(withJSONObject: ["description": draft.description]), grant: management)
        guard value.state == "submitted", let reference = value.reference, DiagnosticWire.matches(reference, "^[A-Za-z0-9-]{1,64}$") else { throw Failure.invalid }
        draft.state = value.state; draft.reference = reference; await DiagnosticStore.shared.save(draft)
    }
    func offer(_ draft: inout DiagnosticStore.Draft) async throws {
        guard let origin = draft.origin, let management = secrets(draft.id).management, draft.consented else { throw Failure.consent }
        let value = try await decoded(Offer.self, origin, path: "/support/v1/handoffs", body: JSONSerialization.data(withJSONObject: ["report_id": draft.id]), grant: management)
        guard UUID(uuidString: value.id)?.uuidString.lowercased() == value.id, DiagnosticWire.matches(value.manual_code, "^[A-Z2-9]{10}$"),
              value.url.hasPrefix(try Self.origin(origin) + "/#handoff="), value.url.utf8.count <= 1024, value.expires_in_ms <= 600_000 else { throw Failure.invalid }
        draft.handoff = value; await DiagnosticStore.shared.save(draft)
    }
    func handoffStatus(_ draft: DiagnosticStore.Draft) async throws -> HandoffStatus {
        guard let origin = draft.origin, let handoff = draft.handoff, let management = secrets(draft.id).management else { throw Failure.invalid }
        return try await decoded(HandoffStatus.self, origin, path: "/support/v1/handoffs/" + handoff.id + "/status", method: "GET", grant: management)
    }
    func approve(_ draft: DiagnosticStore.Draft, comparison: String) async throws {
        guard let origin = draft.origin, let handoff = draft.handoff, let management = secrets(draft.id).management, DiagnosticWire.matches(comparison, "^[A-Z2-9]{6}$") else { throw Failure.invalid }
        _ = try await request(origin, path: "/support/v1/handoffs/" + handoff.id + "/approve", body: JSONSerialization.data(withJSONObject: ["comparison": comparison]), grant: management)
    }
    func delete(_ draft: DiagnosticStore.Draft, keep: Bool = false) async throws {
        guard let origin = draft.origin, let management = secrets(draft.id).management else { return }
        if let handoff = draft.handoff { _ = try await request(origin, path: "/support/v1/handoffs/" + handoff.id, method: "DELETE", body: JSONSerialization.data(withJSONObject: ["keep_draft": keep]), grant: management) }
        else if !keep { _ = try await request(origin, path: "/support/v1/reports/" + draft.id, method: "DELETE", grant: management) }
    }
}
