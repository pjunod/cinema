import Foundation
import XCTest
@testable import plurx

private final class SharedManagementHTTP: URLProtocol {
    static var body = Data()
    static var status = 200
    static var requests: [URLRequest] = []
    static var beforeResponse: (() -> Void)?
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        Self.requests.append(request); Self.beforeResponse?()
        client?.urlProtocol(self, didReceive: HTTPURLResponse(url: request.url!, statusCode: Self.status, httpVersion: nil, headerFields: nil)!, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Self.body); client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}

final class SharedSharingManagementTests: XCTestCase {
    override func tearDown() { SharedManagementHTTP.requests = []; SharedManagementHTTP.status = 200; SharedManagementHTTP.beforeResponse = nil; Session.shared.setCredentials(origin: "", token: nil) }
    private let uuid = "11111111-1111-4111-8111-111111111111"
    private let server = "22222222-2222-4222-8222-222222222222"
    private let epoch = "33333333-3333-4333-8333-333333333333"
    private let precise: Int64 = 9_007_199_254_740_993
    private func client() throws -> SharedSharingManagementClient {
        Session.shared.setCredentials(origin: "https://b.test", token: "b-token")
        let configuration = URLSessionConfiguration.ephemeral; configuration.protocolClasses = [SharedManagementHTTP.self]
        return try SharedSharingManagementClient(testTransport: URLSession(configuration: configuration))
    }
    private func requestJSON() throws -> [String: SharedPlaybackJSON] {
        let request = SharedManagementHTTP.requests.last!
        var bytes = request.httpBody ?? Data()
        if bytes.isEmpty, let stream = request.httpBodyStream {
            stream.open(); defer { stream.close() }; var buffer = [UInt8](repeating: 0, count: 4096)
            while true { let count = stream.read(&buffer, maxLength: buffer.count); if count <= 0 { break }; bytes.append(contentsOf: buffer.prefix(count)) }
        }
        return try JSONDecoder().decode([String: SharedPlaybackJSON].self, from: bytes)
    }
    private func exportWire() -> String { """
        {"exports":[{"grant":{"id":"\(uuid)","recipient_server_id":"\(server)","state":"pending","scope_generation":1,"credential_generation":1,"catalogue_generation":1,"mutation_generation":\(precise),"pending_expires_at_ms":1},"recipient_name":"Recipient B","invitation_id":"\(uuid)","claim_id":"\(epoch)","library_ids":["\(precise)"],"pairing_code":"1234567890abcdef"}],"next":null}
        """ }
    func testActualApprovalAndScopeBodiesPreserveExactIdsAndRequireEnteredCode() async throws {
        let client = try client(); SharedManagementHTTP.body = Data(exportWire().utf8)
        let page = try await client.exports(); let row = page.rows[0]
        XCTAssertEqual(row.grant.mutationGeneration, precise); XCTAssertEqual(row.libraryIds, [String(precise)])
        SharedManagementHTTP.requests = []
        do { try await client.approve(row, enteredCode: ""); XCTFail("approval without entered code") } catch {}
        XCTAssertTrue(SharedManagementHTTP.requests.isEmpty)
        SharedManagementHTTP.body = Data("{\"updated\":true}".utf8)
        try await client.approve(row, enteredCode: "1234567890abcdef")
        XCTAssertEqual(SharedManagementHTTP.requests.last?.url?.host, "b.test")
        XCTAssertEqual(SharedManagementHTTP.requests.last?.url?.path, "/api/v1/sharing/exports/\(uuid)/approve")
        let approval = try requestJSON(); XCTAssertEqual(approval["expected_mutation_generation"], .integer(precise)); XCTAssertEqual(approval["pairing_code"], .string("1234567890abcdef"))
        try await client.scope(row, libraries: [String(precise)])
        let scope = try requestJSON(); XCTAssertEqual(scope["library_ids"], .array([.string(String(precise))])); XCTAssertEqual(scope["expected_mutation_generation"], .integer(precise))
        SharedManagementHTTP.requests = []; SharedManagementHTTP.status = 409; SharedManagementHTTP.body = Data("{\"code\":\"sharing_conflict\",\"message\":\"Refresh generation\"}".utf8)
        do { try await client.scope(row, libraries: []); XCTFail("conflict accepted") } catch {}
        XCTAssertEqual(SharedManagementHTTP.requests.count, 1)
    }
    func testEndpointAndAssignmentReadsFailClosedAndMutationBodyIsBounded() async throws {
        let client = try client()
        let endpoint = SharedSharingEndpoint(ipv4: "100.64.1.2", ipv6: "fd7a:115c:a1e0::1", tsFqdn: "cinema.example.ts.net", port: 8443, spkiSha256: String(repeating: "a", count: 64))
        XCTAssertNoThrow(try endpoint.validate())
        XCTAssertThrowsError(try SharedSharingEndpoint(ipv4: "127.0.0.1", ipv6: nil, tsFqdn: endpoint.tsFqdn, port: 8443, spkiSha256: endpoint.spkiSha256).validate())
        let row = SharedSharingImportSummary(id: uuid, sourceServerId: server, catalogueEpoch: epoch, sourceName: "A", claimId: uuid, remoteGrantId: nil, state: "active", assignmentGeneration: precise, lifecycleGeneration: 3, endpointGeneration: 1, observedEndpointRevision: nil, endpoints: [endpoint])
        SharedManagementHTTP.body = Data("{\"state\":\"active\",\"import_id\":\"\(uuid)\",\"server_id\":\"\(server)\",\"catalogue_epoch\":\"\(epoch)\",\"lifecycle_generation\":3,\"expected_assignment_generation\":\(precise),\"assignments\":[{\"library_id\":\"\(precise)\",\"user_ids\":[\(precise)]}]}".utf8)
        let snapshot = try await client.assignments(row)
        XCTAssertEqual(snapshot.assignments[0].userIds, [precise])
        SharedManagementHTTP.body = Data("{\"updated\":true}".utf8)
        try await client.saveAssignments(snapshot, for: row, groups: snapshot.assignments)
        XCTAssertEqual(try requestJSON()["expected_assignment_generation"], .integer(precise))
        SharedManagementHTTP.requests = []
        let oversized = (0..<64).map { SharedSharingAssignmentGroup(libraryId: String($0), userIds: (0..<256).map { precise + Int64($0) }) }
        do { try await client.saveAssignments(snapshot, for: row, groups: oversized); XCTFail("oversized replacement sent") } catch {}
        XCTAssertTrue(SharedManagementHTTP.requests.isEmpty)
        SharedManagementHTTP.beforeResponse = { Session.shared.setCredentials(origin: "https://b.test", token: "replacement") }
        do { _ = try await client.endpoints(); XCTFail("old account response accepted") } catch {}
    }

    func testAdminSourceLibraryReadBindsFullImportAndAcceptsOnlyExplicitEmptyScope() async throws {
        let client = try client()
        let row = SharedSharingImportSummary(id: uuid, sourceServerId: server, catalogueEpoch: epoch, sourceName: "A", claimId: uuid, remoteGrantId: uuid, state: "active", assignmentGeneration: precise, lifecycleGeneration: 3, endpointGeneration: 1, observedEndpointRevision: nil, endpoints: [])
        func wire(_ libraries: String, generation: Int64? = nil) -> String {
            "{\"state\":\"active\",\"import_id\":\"\(uuid)\",\"server_id\":\"\(server)\",\"catalogue_epoch\":\"\(epoch)\",\"lifecycle_generation\":3,\"expected_assignment_generation\":\(generation ?? precise),\"libraries\":\(libraries)}"
        }
        let libraries = "[{\"library_id\":\"\(precise)\",\"name\":\"Source library\",\"kind\":\"movies\",\"anime\":false}]"
        SharedManagementHTTP.body = Data(wire(libraries).utf8)
        let scope = try await client.sourceLibraries(row)
        XCTAssertEqual(scope.libraries[0].libraryId, String(precise))
        XCTAssertEqual(SharedManagementHTTP.requests.last?.url?.path, "/api/v1/sharing/imports/\(uuid)/libraries")
        let escapedName = String(repeating: "\\u0000", count: 256)
        let maximumScope = "[" + (0..<64).map { "{\"library_id\":\"\($0)\",\"name\":\"\(escapedName)\",\"kind\":\"movies\",\"anime\":false}" }.joined(separator: ",") + "]"
        SharedManagementHTTP.body = Data(wire(maximumScope).utf8)
        let complete = try await client.sourceLibraries(row)
        XCTAssertEqual(complete.libraries.count, 64); XCTAssertEqual(complete.libraries.last?.libraryId, "63")
        SharedManagementHTTP.body = Data(wire("[]").utf8)
        let empty = try await client.sourceLibraries(row); XCTAssertTrue(empty.libraries.isEmpty)
        for corrupt in [wire(libraries, generation: precise + 1), wire(libraries).replacingOccurrences(of: server, with: uuid), wire(libraries).replacingOccurrences(of: "\"library_id\":\"\(precise)\"", with: "\"library_id\":\(precise)"), wire("[" + libraries.dropFirst().dropLast() + "," + libraries.dropFirst().dropLast() + "]")] {
            SharedManagementHTTP.body = Data(corrupt.utf8)
            do { _ = try await client.sourceLibraries(row); XCTFail("unbound/corrupt Source scope accepted") } catch {}
        }
        SharedManagementHTTP.body = Data(wire(libraries.replacingOccurrences(of: "Source library", with: String(repeating: "😀", count: 65))).utf8)
        do { _ = try await client.sourceLibraries(row); XCTFail("Source name byte bound ignored") } catch {}
        SharedManagementHTTP.status = 503; SharedManagementHTTP.body = Data("{}".utf8)
        do { _ = try await client.sourceLibraries(row); XCTFail("offline scope inferred as empty") } catch {}
        SharedManagementHTTP.status = 200; SharedManagementHTTP.body = Data(wire("[]").utf8)
        SharedManagementHTTP.beforeResponse = { Session.shared.setCredentials(origin: "https://b.test", token: "replacement") }
        do { _ = try await client.sourceLibraries(row); XCTFail("old login scope accepted") } catch {}
    }

    func testAssignmentMatrixRetainsOutsideScopeAndMissingViewersUntilExplicitEdit() throws {
        let row = SharedSharingImportSummary(id: uuid, sourceServerId: server, catalogueEpoch: epoch, sourceName: "A", claimId: uuid, remoteGrantId: uuid, state: "active", assignmentGeneration: precise, lifecycleGeneration: 3, endpointGeneration: 1, observedEndpointRevision: nil, endpoints: [])
        let existing = [SharedSharingAssignmentGroup(libraryId: String(precise), userIds: [precise]), SharedSharingAssignmentGroup(libraryId: "7", userIds: [])]
        let snapshot = SharedSharingAssignmentSnapshot(state: "active", importId: uuid, serverId: server, catalogueEpoch: epoch, lifecycleGeneration: 3, expectedAssignmentGeneration: precise, assignments: existing)
        let scope = SharedSharingSourceLibrariesSnapshot(state: "active", importId: uuid, serverId: server, catalogueEpoch: epoch, lifecycleGeneration: 3, expectedAssignmentGeneration: precise, libraries: [SharedSharingSourceLibrary(libraryId: "8", name: "Current", kind: "movies", anime: false)])
        var matrix = try SharedSharingAssignmentMatrix(row: row, assignments: snapshot, scope: scope, viewers: [SharedSharingViewer(id: 1, username: "Current viewer", isAdmin: false)])
        XCTAssertEqual(matrix.groups, existing)
        XCTAssertTrue(matrix.libraries.first { $0.id == String(precise) }!.outsideScope)
        XCTAssertTrue(matrix.viewers.contains { $0.id == precise })
        let requestRevision = matrix.revision
        try matrix.set(library: "8", viewer: 1, enabled: true)
        XCTAssertFalse(matrix.accepts(requestRevision))
        XCTAssertEqual(matrix.groups.prefix(2), existing[...])
        XCTAssertThrowsError(try matrix.set(library: "999", viewer: 1, enabled: true))
        XCTAssertThrowsError(try matrix.removeOutsideScope("8"))
        try matrix.removeOutsideScope(String(precise))
        XCTAssertFalse(matrix.groups.contains { $0.libraryId == String(precise) })
        XCTAssertTrue(matrix.groups.contains { $0.libraryId == "7" && $0.userIds.isEmpty })
    }

    func testEndpointMutationsPreserveExactGenerationsAndRequireExplicitNewPins() async throws {
        let client = try client()
        let endpoint = SharedSharingEndpoint(ipv4: "100.64.1.2", ipv6: nil, tsFqdn: "cinema.example.ts.net", port: 8443, spkiSha256: String(repeating: "a", count: 64))
        SharedManagementHTTP.body = Data("{\"updated\":true}".utf8)
        try await client.saveManifest(expectedRevision: 0, endpoints: [endpoint])
        XCTAssertEqual(SharedManagementHTTP.requests.last?.httpMethod, "PUT")
        XCTAssertEqual(SharedManagementHTTP.requests.last?.url?.path, "/api/v1/sharing/endpoints")
        XCTAssertEqual(try requestJSON()["expected_revision"], .integer(0))
        try await client.saveManifest(expectedRevision: precise, endpoints: [endpoint])
        XCTAssertEqual(try requestJSON()["expected_revision"], .integer(precise))
        let row = SharedSharingImportSummary(id: uuid, sourceServerId: server, catalogueEpoch: epoch, sourceName: "A", claimId: uuid, remoteGrantId: uuid, state: "active", assignmentGeneration: 1, lifecycleGeneration: 3, endpointGeneration: precise, observedEndpointRevision: nil, endpoints: [endpoint])
        let replacement = SharedSharingEndpoint(ipv4: "100.64.1.3", ipv6: nil, tsFqdn: endpoint.tsFqdn, port: 8443, spkiSha256: String(repeating: "b", count: 64))
        SharedManagementHTTP.requests = []
        do { try await client.saveSourceEndpoints(row, endpoints: [replacement], confirmNewPins: false); XCTFail("new pin implicitly confirmed") } catch {}
        XCTAssertTrue(SharedManagementHTTP.requests.isEmpty)
        try await client.saveSourceEndpoints(row, endpoints: [replacement], confirmNewPins: true)
        XCTAssertEqual(SharedManagementHTTP.requests.last?.url?.path, "/api/v1/sharing/imports/\(uuid)/endpoints")
        let wire = try requestJSON(); XCTAssertEqual(wire["expected_endpoint_generation"], .integer(precise)); XCTAssertEqual(wire["confirm_new_pins"], .bool(true))
        XCTAssertEqual(wire["endpoints"], .array([.object(["ipv4": .string(replacement.ipv4), "ipv6": .null, "ts_fqdn": .string(endpoint.tsFqdn), "port": .integer(8443), "spki_sha256": .string(replacement.spkiSha256)])]))
        SharedManagementHTTP.requests = []; SharedManagementHTTP.status = 409; SharedManagementHTTP.body = Data("{\"code\":\"sharing_conflict\",\"message\":\"Reload endpoint generation\"}".utf8)
        do { try await client.saveSourceEndpoints(row, endpoints: [endpoint], confirmNewPins: false); XCTFail("stale endpoint save accepted") } catch {}
        XCTAssertEqual(SharedManagementHTTP.requests.count, 1)
        SharedManagementHTTP.status = 200; SharedManagementHTTP.body = Data("{\"updated\":true}".utf8)
        SharedManagementHTTP.beforeResponse = { Session.shared.setCredentials(origin: "https://b.test", token: "replacement") }
        do { try await client.saveManifest(expectedRevision: precise, endpoints: [endpoint]); XCTFail("old account completion accepted") } catch {}
    }
    func testEndpointDraftRejectsPublicTargetsAndMalformedFieldsBeforeAnyRequest() async throws {
        let client = try client()
        let endpoint = SharedSharingEndpoint(ipv4: "100.127.255.254", ipv6: "fd7a:115c:a1e0::1", tsFqdn: "cinema.example.ts.net", port: 65535, spkiSha256: String(repeating: "a", count: 64))
        XCTAssertEqual(try SharedSharingEndpointFields(endpoint).validated(), endpoint)
        var fields = SharedSharingEndpointFields(endpoint); fields.port = "065535"; XCTAssertThrowsError(try fields.validated())
        fields = SharedSharingEndpointFields(endpoint); fields.ipv4 = "100.128.0.0"; XCTAssertThrowsError(try fields.validated())
        fields = SharedSharingEndpointFields(endpoint); fields.ipv6 = "fd00::1"; XCTAssertThrowsError(try fields.validated())
        fields = SharedSharingEndpointFields(endpoint); fields.tsFqdn = "cinema.ts.net"; XCTAssertThrowsError(try fields.validated())
        fields = SharedSharingEndpointFields(endpoint); fields.pin = String(repeating: "A", count: 64); XCTAssertThrowsError(try fields.validated())
        SharedManagementHTTP.requests = []
        for endpoints in [[], Array(repeating: endpoint, count: 5), [SharedSharingEndpoint(ipv4: "127.0.0.1", ipv6: nil, tsFqdn: endpoint.tsFqdn, port: 8443, spkiSha256: endpoint.spkiSha256)]] {
            do { try await client.saveManifest(expectedRevision: 0, endpoints: endpoints); XCTFail("invalid endpoint mutation sent") } catch {}
        }
        XCTAssertTrue(SharedManagementHTTP.requests.isEmpty)
    }

    func testCurrentManagementAuthorizationRefusalsRetireAllDraftsWithoutChangingSession() async throws {
        let client = try client()
        for status in [401, 403] {
            let first = SharedSharingSecretDraft(); let second = SharedSharingSecretDraft()
            first.edit(invitation: "first-secret"); second.edit(pairingCode: "second-secret")
            let auth = Session.shared.playbackAuthorization
            SharedManagementHTTP.status = status; SharedManagementHTTP.body = status == 403 ? Data(repeating: 32, count: 131_073) : Data("{}".utf8)
            do { _ = try await client.endpoints(); XCTFail("refused authorization accepted") } catch {}
            XCTAssertNil(first.snapshot()); XCTAssertNil(second.snapshot())
            let unchanged = Session.shared.playbackAuthorization
            XCTAssertEqual(unchanged.generation, auth.generation); XCTAssertEqual(unchanged.token, auth.token); XCTAssertEqual(unchanged.origin, auth.origin)
            first.leave(); second.leave()
        }
    }
    func testOldAuthorizationRefusalCannotRetireNewAccountDraftAndLeaveRemovesRegistration() async throws {
        let old = try client(); let oldAuth = Session.shared.playbackAuthorization
        let left = SharedSharingSecretDraft(); left.edit(invitation: "retired-on-leave"); left.leave()
        func drainPresentation() async {
            let drained = expectation(description: "presentation queue drained")
            DispatchQueue.main.async { drained.fulfill() }; await fulfillment(of: [drained], timeout: 2)
        }
        await drainPresentation()
        let notificationLock = NSLock(); var notifications = 0
        let subscription = left.objectWillChange.sink { notificationLock.lock(); notifications += 1; notificationLock.unlock() }
        SharedSharingSecretDraft.retireAuthorization(generation: oldAuth.generation)
        await drainPresentation()
        let count = notificationLock.withLock { notifications }
        XCTAssertEqual(count, 0, "left draft still received registry invalidations"); subscription.cancel()
        var replacement: SharedSharingSecretDraft?
        SharedManagementHTTP.status = 401; SharedManagementHTTP.body = Data("{}".utf8)
        SharedManagementHTTP.beforeResponse = {
            Session.shared.setCredentials(origin: "https://b.test", token: "replacement")
            replacement = SharedSharingSecretDraft(); replacement?.edit(invitation: "new-account-secret")
        }
        do { _ = try await old.endpoints(); XCTFail("old account refusal accepted as current") } catch {}
        XCTAssertNil(left.snapshot()); XCTAssertEqual(replacement?.snapshot()?.invitation, "new-account-secret")
        SharedSharingSecretDraft.retireAuthorization(generation: oldAuth.generation)
        XCTAssertEqual(replacement?.snapshot()?.invitation, "new-account-secret")
        replacement?.leave()
    }

    func testActualInvitationImportRePairRotationAndExplicitDisconnectRoutes() async throws {
        let client = try client(); let token = "cinema-share-v1:Zml4dHVyZQ"
        SharedManagementHTTP.body = Data("{\"id\":\"\(uuid)\",\"invitation\":\"\(token)\",\"expires_at_ms\":1000}".utf8)
        _ = try await client.invite(libraries: [String(precise)])
        XCTAssertEqual(SharedManagementHTTP.requests.last?.url?.path, "/api/v1/sharing/invitations")
        XCTAssertEqual(try requestJSON()["library_ids"], .array([.string(String(precise))]))
        let wire = """
        {"import":{"id":"\(uuid)","source_server_id":"\(server)","catalogue_epoch":"\(epoch)","source_name":"Source A","claim_id":"\(uuid)","remote_grant_id":"44444444-4444-4444-8444-444444444444","state":"active","assignment_generation":2,"lifecycle_generation":4,"endpoint_generation":2,"observed_endpoint_revision":null,"endpoints":[{"ipv4":"100.64.1.2","ipv6":null,"ts_fqdn":"cinema.example.ts.net","port":8443,"spki_sha256":"\(String(repeating: "a", count: 64))"}]},"pairing_code":"1234567890abcdef"}
        """
        SharedManagementHTTP.body = Data(wire.utf8)
        let imported = try await client.importSource(invitation: token)
        XCTAssertEqual(try requestJSON()["invitation"], .string(token))
        _ = try await client.rePair(imported.import, invitation: token)
        XCTAssertEqual(SharedManagementHTTP.requests.last?.url?.path, "/api/v1/sharing/imports/\(uuid)/re-pair")
        XCTAssertEqual(try requestJSON()["expected_lifecycle_generation"], .integer(4))
        _ = try await client.rotate(imported.import)
        XCTAssertEqual(try requestJSON(), [:])
        SharedManagementHTTP.body = Data(wire.replacingOccurrences(of: server, with: "44444444-4444-4444-8444-444444444444").utf8)
        do { _ = try await client.rePair(imported.import, invitation: token); XCTFail("re-pair changed Source identity") } catch {}
        SharedManagementHTTP.requests = []
        SharedManagementHTTP.body = Data("{\"disabled\":true}".utf8); try await client.disconnect(uuid)
        XCTAssertEqual(SharedManagementHTTP.requests.last?.httpMethod, "DELETE")
        XCTAssertEqual(SharedManagementHTTP.requests.last?.url?.path, "/api/v1/sharing/imports/\(uuid)")
        SharedManagementHTTP.body = Data("{\"revoked\":true}".utf8); try await client.revoke(uuid)
        SharedManagementHTTP.body = Data("{\"cancelled\":true}".utf8); try await client.cancelInvitation(uuid)
        XCTAssertEqual(SharedManagementHTTP.requests.count, 3)
    }
    func testActualAuthorizationChangesNotifyOutsideLockAndRemovalStopsNotifications() {
        Session.shared.setCredentials(origin: "https://b.test", token: "b-token")
        var values: [UInt64] = []
        let observer = Session.shared.observeAuthorizationChanges { generation in
            let read = self.expectation(description: "cross-thread authorization read")
            DispatchQueue.global().async {
                XCTAssertEqual(Session.shared.playbackAuthorization.generation, generation)
                read.fulfill()
            }
            self.wait(for: [read], timeout: 2)
            values.append(generation)
        }
        Session.shared.setCredentials(origin: "https://b.test", token: "b-token")
        XCTAssertTrue(values.isEmpty)
        Session.shared.setCredentials(origin: "https://b.test", token: "new-token")
        XCTAssertEqual(values, [observer.generation + 1])
        Session.shared.removeAuthorizationObserver(observer.id)
        Session.shared.setCredentials(origin: "https://other.test", token: "other-token")
        XCTAssertEqual(values.count, 1)
    }
    func testTransientPairingSecretsRetireOnLogoutLeaveAndRegistrationRace() {
        Session.shared.setCredentials(origin: "https://b.test", token: "b-token")
        let draft = SharedSharingSecretDraft(); draft.edit(invitation: "secret", pairingCode: "1234567890abcdef")
        let revision = draft.snapshot()!.revision
        Session.shared.setCredentials(origin: "https://b.test", token: "b-token")
        XCTAssertTrue(draft.accepts(revision))
        Session.shared.setCredentials(origin: "https://b.test", token: nil)
        XCTAssertNil(draft.snapshot()); XCTAssertFalse(draft.accepts(revision))
        Session.shared.setCredentials(origin: "https://b.test", token: "b-token")
        XCTAssertNil(draft.snapshot())
        let leaving = SharedSharingSecretDraft(); leaving.edit(invitation: "other-secret"); leaving.leave()
        XCTAssertNil(leaving.snapshot())
        let raced = SharedSharingSecretDraft(beforeObserver: { Session.shared.setCredentials(origin: "https://b.test", token: "replacement") })
        XCTAssertNil(raced.snapshot())
        draft.leave(); raced.leave()
    }
    func testOlderAcknowledgementCannotClearNewerPairingEdit() {
        Session.shared.setCredentials(origin: "https://b.test", token: "b-token")
        let draft = SharedSharingSecretDraft(); defer { draft.leave() }
        draft.edit(invitation: "old-secret"); let old = draft.snapshot()!
        draft.edit(invitation: "new-secret"); draft.clear(ifRevision: old.revision)
        XCTAssertEqual(draft.snapshot()?.invitation, "new-secret"); XCTAssertFalse(draft.accepts(old.revision))
        draft.clear(ifRevision: draft.snapshot()!.revision); XCTAssertEqual(draft.snapshot()?.invitation, "")
    }
}
