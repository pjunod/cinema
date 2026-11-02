import Foundation
import Security
import XCTest
@testable import plurx

private final class TrustTestVault: TokenStoring {
    var token: String?
    var acceptsWrites = true
    var acceptsClears = true
    var clears = 0
    var writes = 0
    func read() -> String? { token }
    func write(_ token: String) -> Bool {
        writes += 1
        guard acceptsWrites else { return false }
        self.token = token
        return true
    }
    func clear() { clears += 1; if acceptsClears { token = nil } }
}

private final class TrustTestProtectionSpace: URLProtectionSpace, @unchecked Sendable {
    private let suppliedTrust: SecTrust?
    init(host: String, port: Int, trust: SecTrust?) {
        suppliedTrust = trust
        super.init(host: host, port: port, protocol: "https", realm: nil,
                   authenticationMethod: NSURLAuthenticationMethodServerTrust)
    }
    required init?(coder: NSCoder) { return nil }
    override var serverTrust: SecTrust? { suppliedTrust }
}

private final class TrustTestChallengeSender: NSObject, URLAuthenticationChallengeSender {
    func use(_ credential: URLCredential, for challenge: URLAuthenticationChallenge) {}
    func continueWithoutCredential(for challenge: URLAuthenticationChallenge) {}
    func cancel(_ challenge: URLAuthenticationChallenge) {}
}

final class ServerTrustTests: XCTestCase {
    // Fixed time and static certificates make expiration independent of the
    // simulator clock. Fixtures contain public certificates only, no keys.
    // Valid leaves run 2026-10-01 through 2026-12-31; the expired/future
    // variants use the same dates in 2020/2030. RSA-2048, SHA-256, localhost
    // SAN, and serverAuth EKU match the private-server TLS requirements.
    private let verificationDate = Date(timeIntervalSince1970: 1_793_548_800) // 2026-11-01 UTC

    private func certificate(_ fixture: String) throws -> SecCertificate {
        let data = try XCTUnwrap(Data(base64Encoded: fixture, options: .ignoreUnknownCharacters))
        return try XCTUnwrap(SecCertificateCreateWithData(nil, data as CFData))
    }

    private func trust(_ fixture: String, host: String = "media.test", anchor: String? = nil) throws -> SecTrust {
        let leaf = try certificate(fixture)
        var created: SecTrust?
        XCTAssertEqual(SecTrustCreateWithCertificates([leaf] as CFArray,
            SecPolicyCreateSSL(true, host as CFString), &created), errSecSuccess)
        let result = try XCTUnwrap(created)
        XCTAssertEqual(SecTrustSetNetworkFetchAllowed(result, false), errSecSuccess)
        XCTAssertEqual(SecTrustSetVerifyDate(result, verificationDate as CFDate), errSecSuccess)
        if let anchor {
            let root = try certificate(anchor)
            XCTAssertEqual(SecTrustSetAnchorCertificates(result, [root] as CFArray), errSecSuccess)
            XCTAssertEqual(SecTrustSetAnchorCertificatesOnly(result, true), errSecSuccess)
        }
        return result
    }

    private func challenge(_ fixture: String, host: String = "media.test", port: Int = 32400,
                           anchor: String? = nil) throws -> URLAuthenticationChallenge {
        let space = TrustTestProtectionSpace(host: host, port: port,
            trust: try trust(fixture, host: host, anchor: anchor))
        return URLAuthenticationChallenge(protectionSpace: space, proposedCredential: nil,
            previousFailureCount: 0, failureResponse: nil, error: nil, sender: TrustTestChallengeSender())
    }

    func testSelectedSelfSignedServerEnrollsWithoutMatchingDNSName() throws {
        let storage = TrustTestVault()
        var origins: [String] = []
        let evaluator = ServerTrust(vault: { origins.append($0); return storage }, now: { self.verificationDate })
        evaluator.allowFirstUse(origin: "https://media.test:32400")
        let (disposition, credential) = evaluator.disposition(for: try challenge(Self.privateA))
        XCTAssertEqual(disposition, .useCredential)
        XCTAssertNotNil(credential)
        XCTAssertEqual(origins, ["https://media.test:32400"])
        XCTAssertNotNil(storage.token)
        XCTAssertEqual(storage.writes, 1)
    }

    func testPinSurvivesNewEvaluatorWithoutReenrollment() throws {
        let storage = TrustTestVault()
        let first = ServerTrust(vault: { _ in storage }, now: { self.verificationDate })
        first.allowFirstUse(origin: "https://media.test:32400")
        XCTAssertEqual(first.disposition(for: try challenge(Self.privateA)).0, .useCredential)
        let second = ServerTrust(vault: { _ in storage }, now: { self.verificationDate })
        XCTAssertEqual(second.disposition(for: try challenge(Self.privateA)).0, .useCredential)
        XCTAssertEqual(storage.writes, 1)
    }

    func testRememberedCertificateStillExpires() throws {
        let storage = TrustTestVault()
        let first = ServerTrust(vault: { _ in storage }, now: { self.verificationDate })
        first.allowFirstUse(origin: "https://media.test:32400")
        XCTAssertEqual(first.disposition(for: try challenge(Self.privateA)).0, .useCredential)
        let remembered = storage.token
        let afterExpiration = Date(timeIntervalSince1970: 1_798_761_600) // 2027-01-01 UTC
        let later = ServerTrust(vault: { _ in storage }, now: { afterExpiration })
        let result = later.disposition(for: try challenge(Self.privateA))
        XCTAssertEqual(result.0, .cancelAuthenticationChallenge)
        XCTAssertNil(result.1)
        XCTAssertEqual(storage.token, remembered)
        XCTAssertEqual(storage.writes, 1)
    }

    func testChangedCertificateCannotReplacePinEvenAfterSelectingAgain() throws {
        let storage = TrustTestVault()
        let evaluator = ServerTrust(vault: { _ in storage }, now: { self.verificationDate })
        evaluator.allowFirstUse(origin: "https://media.test:32400")
        XCTAssertEqual(evaluator.disposition(for: try challenge(Self.privateA)).0, .useCredential)
        let remembered = storage.token
        evaluator.allowFirstUse(origin: "https://media.test:32400")
        let (disposition, credential) = evaluator.disposition(for: try challenge(Self.privateB))
        XCTAssertEqual(disposition, .cancelAuthenticationChallenge)
        XCTAssertNil(credential)
        XCTAssertEqual(storage.token, remembered)
        XCTAssertEqual(storage.writes, 1)
    }

    func testUnselectedSelfSignedHostCannotEnroll() throws {
        let storage = TrustTestVault()
        let evaluator = ServerTrust(vault: { _ in storage }, now: { self.verificationDate })
        evaluator.allowFirstUse(origin: "https://selected.test:32400")
        XCTAssertEqual(evaluator.disposition(for: try challenge(Self.privateA)).0, .cancelAuthenticationChallenge)
        XCTAssertNil(storage.token)
        XCTAssertEqual(storage.writes, 0)
    }

    func testPinsAreIndependentAcrossPorts() throws {
        var stores: [String: TrustTestVault] = [:]
        let evaluator = ServerTrust(vault: { origin in
            if let existing = stores[origin] { return existing }
            let storage = TrustTestVault(); stores[origin] = storage; return storage
        }, now: { self.verificationDate })
        evaluator.allowFirstUse(origin: "https://media.test:32400")
        XCTAssertEqual(evaluator.disposition(for: try challenge(Self.privateA)).0, .useCredential)
        XCTAssertEqual(evaluator.disposition(for: try challenge(Self.privateB, port: 8443)).0, .useCredential)
        XCTAssertEqual(stores.count, 2)
        XCTAssertNotEqual(stores["https://media.test:32400"]?.token, stores["https://media.test:8443"]?.token)
        XCTAssertEqual(evaluator.disposition(for: try challenge(Self.privateB)).0, .cancelAuthenticationChallenge)
    }

    func testFailedDurablePinWriteRefusesConnection() throws {
        let storage = TrustTestVault(); storage.acceptsWrites = false
        let evaluator = ServerTrust(vault: { _ in storage }, now: { self.verificationDate })
        evaluator.allowFirstUse(origin: "https://media.test:32400")
        let result = evaluator.disposition(for: try challenge(Self.privateA))
        XCTAssertEqual(result.0, .cancelAuthenticationChallenge)
        XCTAssertNil(result.1)
        XCTAssertNil(storage.token)
        XCTAssertEqual(storage.writes, 1)
    }

    func testExpiredCertificateIsNeverEnrolled() throws { try assertPrivateCertificateRefused(Self.expired) }
    func testNotYetValidCertificateIsNeverEnrolled() throws { try assertPrivateCertificateRefused(Self.future) }
    func testClientOnlyCertificateIsNeverEnrolled() throws { try assertPrivateCertificateRefused(Self.clientOnly) }

    private func assertPrivateCertificateRefused(_ fixture: String) throws {
        let storage = TrustTestVault()
        let evaluator = ServerTrust(vault: { _ in storage }, now: { self.verificationDate })
        evaluator.allowFirstUse(origin: "https://media.test:32400")
        let result = evaluator.disposition(for: try challenge(fixture))
        XCTAssertEqual(result.0, .cancelAuthenticationChallenge)
        XCTAssertNil(result.1)
        XCTAssertNil(storage.token)
        XCTAssertEqual(storage.writes, 0)
    }

    func testMalformedCertificateAndMissingServerTrustAreRefused() {
        XCTAssertNil(SecCertificateCreateWithData(nil, Data([0x30, 0x01, 0x00]) as CFData))
        let evaluator = ServerTrust(vault: { _ in TrustTestVault() }, now: { self.verificationDate })
        evaluator.allowFirstUse(origin: "https://media.test:32400")
        let space = TrustTestProtectionSpace(host: "media.test", port: 32400, trust: nil)
        let challenge = URLAuthenticationChallenge(protectionSpace: space, proposedCredential: nil,
            previousFailureCount: 0, failureResponse: nil, error: nil, sender: TrustTestChallengeSender())
        let result = evaluator.disposition(for: challenge)
        XCTAssertEqual(result.0, .cancelAuthenticationChallenge)
        XCTAssertNil(result.1)
    }

    func testSystemTrustedChainUsesDefaultValidationAndNeverPins() throws {
        let storage = TrustTestVault()
        let evaluator = ServerTrust(vault: { _ in storage }, now: { self.verificationDate })
        let challenge = try challenge(Self.publicLeaf, host: "localhost", anchor: Self.publicRoot)
        XCTAssertTrue(SecTrustEvaluateWithError(try XCTUnwrap(challenge.protectionSpace.serverTrust), nil))
        let result = evaluator.disposition(for: challenge)
        XCTAssertEqual(result.0, .performDefaultHandling)
        XCTAssertNil(result.1)
        XCTAssertNil(storage.token)
        XCTAssertEqual(storage.writes, 0)
    }

    func testOtherAuthenticationMethodsKeepDefaultHandling() {
        let space = URLProtectionSpace(host: "media.test", port: 32400, protocol: "https", realm: nil,
                                       authenticationMethod: NSURLAuthenticationMethodHTTPBasic)
        let challenge = URLAuthenticationChallenge(protectionSpace: space, proposedCredential: nil,
            previousFailureCount: 0, failureResponse: nil, error: nil, sender: TrustTestChallengeSender())
        let evaluator = ServerTrust(vault: { _ in XCTFail("non-TLS challenge accessed pin storage"); return TrustTestVault() })
        let result = evaluator.disposition(for: challenge)
        XCTAssertEqual(result.0, .performDefaultHandling)
        XCTAssertNil(result.1)
    }

    func testResetCannotClearPinWithoutAnObservedReplacement() throws {
        let storage = TrustTestVault()
        let evaluator = ServerTrust(vault: { _ in storage }, now: { self.verificationDate })
        evaluator.allowFirstUse(origin: "https://media.test:32400")
        XCTAssertFalse(evaluator.forgetChangedIdentity(origin: "https://media.test:32400"))
        XCTAssertEqual(evaluator.disposition(for: try challenge(Self.privateA)).0, .useCredential)
        let remembered = storage.token
        XCTAssertFalse(evaluator.forgetChangedIdentity(origin: "https://media.test:32400"))
        XCTAssertEqual(storage.token, remembered)
        XCTAssertEqual(storage.clears, 0)
        XCTAssertNil(evaluator.changedOrigin(for: "http://media.test:32400"))
        XCTAssertNil(evaluator.failureMessage(for: "http://media.test:32400"))
    }

    func testObservedReplacementReportsHTTPSOriginAndExplicitResetEnrollsNewIdentity() throws {
        let storage = TrustTestVault()
        let evaluator = ServerTrust(vault: { _ in storage }, now: { self.verificationDate })
        evaluator.allowFirstUse(origin: "http://media.test:32400")
        XCTAssertEqual(evaluator.disposition(for: try challenge(Self.privateA, port: 32403)).0, .useCredential)
        let oldPin = storage.token
        XCTAssertEqual(evaluator.disposition(for: try challenge(Self.privateB, port: 32403)).0, .cancelAuthenticationChallenge)
        XCTAssertEqual(evaluator.changedOrigin(for: "http://MEDIA.test:32400"), "https://media.test:32403")
        XCTAssertNotNil(evaluator.failureMessage(for: "http://media.test:32400"))
        XCTAssertNil(evaluator.failureMessage(for: "https://unrelated.test:32403"))
        XCTAssertFalse(evaluator.forgetChangedIdentity(origin: "http://media.test:32400"))
        XCTAssertEqual(storage.token, oldPin)
        XCTAssertTrue(evaluator.forgetChangedIdentity(origin: "https://media.test:32403"))
        XCTAssertNil(storage.token)
        XCTAssertEqual(storage.clears, 1)
        XCTAssertNil(evaluator.changedOrigin(for: "http://media.test:32400"))
        XCTAssertNil(evaluator.failureMessage(for: "http://media.test:32400"))
        XCTAssertEqual(evaluator.disposition(for: try challenge(Self.privateB, port: 32403)).0, .useCredential)
        XCTAssertNotEqual(storage.token, oldPin)
        XCTAssertEqual(storage.writes, 2)
        XCTAssertEqual(evaluator.disposition(for: try challenge(Self.privateA, port: 32403)).0, .cancelAuthenticationChallenge)
        XCTAssertEqual(storage.writes, 2)
    }

    func testResetRequiresDurableClearAndUnchangedRememberedPin() throws {
        let storage = TrustTestVault()
        let evaluator = ServerTrust(vault: { _ in storage }, now: { self.verificationDate })
        evaluator.allowFirstUse(origin: "https://media.test:32400")
        XCTAssertEqual(evaluator.disposition(for: try challenge(Self.privateA)).0, .useCredential)
        XCTAssertEqual(evaluator.disposition(for: try challenge(Self.privateB)).0, .cancelAuthenticationChallenge)
        let remembered = storage.token
        storage.acceptsClears = false
        XCTAssertFalse(evaluator.forgetChangedIdentity(origin: "https://media.test:32400"))
        XCTAssertEqual(storage.token, remembered)
        XCTAssertEqual(evaluator.changedOrigin(for: "https://media.test:32400"), "https://media.test:32400")
        storage.acceptsClears = true
        storage.token = "a-pin-published-by-another-owner"
        XCTAssertFalse(evaluator.forgetChangedIdentity(origin: "https://media.test:32400"))
        XCTAssertEqual(storage.token, "a-pin-published-by-another-owner")
        XCTAssertEqual(storage.clears, 1)
    }

    func testReplacementFailuresAndResetsAreIndependentForEachHost() throws {
        var stores: [String: TrustTestVault] = [:]
        let evaluator = ServerTrust(vault: { origin in
            if let existing = stores[origin] { return existing }
            let storage = TrustTestVault(); stores[origin] = storage; return storage
        }, now: { self.verificationDate })
        evaluator.allowFirstUse(origin: "https://alpha.test:32400")
        evaluator.allowFirstUse(origin: "https://beta.test:32400")
        XCTAssertEqual(evaluator.disposition(for: try challenge(Self.privateA, host: "alpha.test")).0, .useCredential)
        XCTAssertEqual(evaluator.disposition(for: try challenge(Self.privateB, host: "beta.test")).0, .useCredential)
        XCTAssertEqual(evaluator.disposition(for: try challenge(Self.privateB, host: "alpha.test")).0, .cancelAuthenticationChallenge)
        XCTAssertNil(evaluator.changedOrigin(for: "http://beta.test:32400"))
        XCTAssertEqual(evaluator.disposition(for: try challenge(Self.privateA, host: "beta.test")).0, .cancelAuthenticationChallenge)
        let betaPin = stores["https://beta.test:32400"]?.token
        XCTAssertTrue(evaluator.forgetChangedIdentity(origin: "https://alpha.test:32400"))
        XCTAssertNil(evaluator.changedOrigin(for: "https://alpha.test:32400"))
        XCTAssertEqual(evaluator.changedOrigin(for: "http://beta.test:32400"), "https://beta.test:32400")
        XCTAssertEqual(stores["https://beta.test:32400"]?.token, betaPin)
        XCTAssertEqual(stores["https://beta.test:32400"]?.clears, 0)
    }

    func testHTTPAndSystemTrustedMediaRemainDirectUntilPrivateIdentityIsLearned() throws {
        var stores: [String: TrustTestVault] = [:]
        let evaluator = ServerTrust(vault: { origin in
            if let existing = stores[origin] { return existing }
            let storage = TrustTestVault(); stores[origin] = storage; return storage
        }, now: { self.verificationDate })
        let http = URL(string: "http://media.test:32400/movie.m3u8")!
        let https = URL(string: "https://media.test:32403/movie.m3u8")!
        XCTAssertFalse(evaluator.requiresMediaProxy(for: http))
        XCTAssertFalse(evaluator.requiresMediaProxy(for: https))
        evaluator.allowFirstUse(origin: "http://media.test:32400")
        XCTAssertFalse(evaluator.requiresMediaProxy(for: http))
        XCTAssertFalse(evaluator.requiresMediaProxy(for: https))
        let publicChallenge = try challenge(Self.publicLeaf, host: "localhost", anchor: Self.publicRoot)
        XCTAssertEqual(evaluator.disposition(for: publicChallenge).0, .performDefaultHandling)
        XCTAssertFalse(evaluator.requiresMediaProxy(for: URL(string: "https://localhost:32400/movie.m3u8")!))
        XCTAssertEqual(evaluator.disposition(for: try challenge(Self.privateA, port: 32403)).0, .useCredential)
        XCTAssertTrue(evaluator.requiresMediaProxy(for: https))
        XCTAssertTrue(evaluator.requiresMediaProxy(for: http))
        XCTAssertFalse(evaluator.requiresMediaProxy(for: URL(string: "https://unrelated.test:32403/movie.m3u8")!))
    }

    func testPersistedHTTPSPinRequiresProxyBeforeNewEvaluatorAcceptsChallenge() throws {
        var stores: [String: TrustTestVault] = [:]
        let vault: (String) -> any TokenStoring = { origin in
            if let existing = stores[origin] { return existing }
            let storage = TrustTestVault(); stores[origin] = storage; return storage
        }
        let first = ServerTrust(vault: vault, now: { self.verificationDate })
        first.allowFirstUse(origin: "https://media.test:32403")
        XCTAssertEqual(first.disposition(for: try challenge(Self.privateA, port: 32403)).0, .useCredential)
        let second = ServerTrust(vault: vault, now: { self.verificationDate })
        XCTAssertTrue(second.requiresMediaProxy(for: URL(string: "https://media.test:32403/movie.m3u8")!))
        XCTAssertFalse(second.requiresMediaProxy(for: URL(string: "https://media.test:443/movie.m3u8")!))
        XCTAssertFalse(second.requiresMediaProxy(for: URL(string: "http://unrelated.test:32400/movie.m3u8")!))
    }

    private static let privateA = """
        MIIC7jCCAdagAwIBAgIBATANBgkqhkiG9w0BAQsFADAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwHhcNMjYxMDAxMDAwMDAwWhcNMjYx
        MjMxMDAwMDAwWjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQCj8sikOTS+ZhcQ
        DFLG9kzMioPOta7Ght1gPUSRK9UBJlSHtPFlJFl2pwHKL8tKC8gNy0lB3MyWTjayaBZElgGXPgZTae56xrPRzkVpnNo4CrkEyZ0g
        gHI+VmQa7gbvCdM0AF/GILQxhxUD3o05HBwGDNLr7EAbk3AVy63L9Koc7qIevFsin9sTBk+aE+m8yihRJKYhMXBodjw8Lx98HJuz
        062OV15ynXk2s6SIhUpg2b6Kj3UqmMv2aoSzA8X7Rl+LOm+EWQCKBQRh+4d0V1Un/Rny7qqbj6930HVS298thB7JFHcxWecZmhsV
        jE9kAavET8o4qJJdPGxmiwluFqzVAgMBAAGjSzBJMAwGA1UdEwEB/wQCMAAwDgYDVR0PAQH/BAQDAgWgMBMGA1UdJQQMMAoGCCsG
        AQUFBwMBMBQGA1UdEQQNMAuCCWxvY2FsaG9zdDANBgkqhkiG9w0BAQsFAAOCAQEAXPTdAYNSIWvLJaLeRPn38nt73hSYh3R0XTio
        w/AvcWFNjHfwUou7BNRFeIWBZyxoe2BkkAQeqQBgWBQtQwqYgKP4cmgi6DEDKwZA9oiKoYQtCMfr9VwbyihBwz4P/Nv/tQy54Tj7
        VbHKSNVKY1P6GRrWZ6Sn6WDUBIm8Z4EDx8yba8PF5lg8wIX5pQXl4jRCH/j7eeBWX6Okjx1RYnpVPH5n69x4VKnotlNMxMd1SQvV
        Fl4P0AELkzHc09L5Dxrnn3F0Emla75+riIRvgyNgyN0P4/NPo0oh5uoOESNku28717okwDDD2LSewGklXD7M/+D9gz7sZ3avJN41
        v8XvcA==
        """

    private static let privateB = """
        MIIC7jCCAdagAwIBAgIBAjANBgkqhkiG9w0BAQsFADAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwHhcNMjYxMDAxMDAwMDAwWhcNMjYx
        MjMxMDAwMDAwWjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQDGyZ5IlHaXUeCE
        KCgZJnvaxK9m4ImnZvLYBqROQhgrDCb9yjoOd0NtIWHl3WlS7bXCCVYNCCoe/kxS8IkjQDBDcPrN3R6Bp/45/1LEDHDXxJclZ+GH
        2GZ+D77snJa9BajgFBjAsue2I6u9lYdliMCjJo0vdWsB8l/fY0eyesdSCg7KjHiaI35psmb4Ce0N4xF1+PPX7r1b88lD+LNKhCLK
        2EkWGeHq5+neuin5qCiy3sJ84/JhJStYTw5/i3IZDAD+QfADp4rze47hwyKWDyyd45vNSKBJ9rORZxnIWwrwXdRxlaK6JsCkFWxP
        DU5/ssNodXrQfaLt2m17GbFN5QxzAgMBAAGjSzBJMAwGA1UdEwEB/wQCMAAwDgYDVR0PAQH/BAQDAgWgMBMGA1UdJQQMMAoGCCsG
        AQUFBwMBMBQGA1UdEQQNMAuCCWxvY2FsaG9zdDANBgkqhkiG9w0BAQsFAAOCAQEAWVvSjNzg3a/1cZrkQ5awxY73CW/td7F65gIi
        +J6YFQIwGu0ooEZM8Ak5UdHzH/LnQeESc/wuh5KUDcKs5JWIHvGa8gDACwYj3S/y4q03gtrrDet7wV3OvZLnBEHYbxXy+upgtNY6
        i0EoWpqLWi0mFmI1te0PG0aD0+xB0OVDw+WWqrzzkylIfVK3zK6U189iJyccUNG26+tDTBLztupqdftdY/jS6qSyzYEx9TgdR0M9
        DArli70ox+fO3SeT8QGfmCoL7mnOwCG3W9swVZnZys2yOCraZJhy4f7yhcERcvS/O7shbedSNvcvu0X9JapIDMFtMbdTXvuXtT9e
        BsMXfA==
        """

    private static let expired = """
        MIIC7jCCAdagAwIBAgIBAzANBgkqhkiG9w0BAQsFADAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwHhcNMjAxMDAxMDAwMDAwWhcNMjAx
        MjMxMDAwMDAwWjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQCj8sikOTS+ZhcQ
        DFLG9kzMioPOta7Ght1gPUSRK9UBJlSHtPFlJFl2pwHKL8tKC8gNy0lB3MyWTjayaBZElgGXPgZTae56xrPRzkVpnNo4CrkEyZ0g
        gHI+VmQa7gbvCdM0AF/GILQxhxUD3o05HBwGDNLr7EAbk3AVy63L9Koc7qIevFsin9sTBk+aE+m8yihRJKYhMXBodjw8Lx98HJuz
        062OV15ynXk2s6SIhUpg2b6Kj3UqmMv2aoSzA8X7Rl+LOm+EWQCKBQRh+4d0V1Un/Rny7qqbj6930HVS298thB7JFHcxWecZmhsV
        jE9kAavET8o4qJJdPGxmiwluFqzVAgMBAAGjSzBJMAwGA1UdEwEB/wQCMAAwDgYDVR0PAQH/BAQDAgWgMBMGA1UdJQQMMAoGCCsG
        AQUFBwMBMBQGA1UdEQQNMAuCCWxvY2FsaG9zdDANBgkqhkiG9w0BAQsFAAOCAQEAm/4wbJCz2cYIdSfriB09C48RKvTi1LaGA/01
        lwU4e6I/RnBZWhbxFOFwz2/SXZYAjvvSK7Cfoe3A73SARhvllQVWhOoqqp64JY4nCW60RKbsbIH0ZtfHB4PNnKNM1djJrw5GcB0M
        WEZ80NdfyCIWvC6lq+UvHVHMJ1Q3Dt9/Wb/Ad0C968/L4/QU9MJ2Sj7R09x+vH3wF8wNaQpE8CGI04nZlNxwbmvAd8pJwTOowbyP
        gHfTQkjbHto0Bk/+CF56toxhzd6TA35wyY5NwE0AEX70fNpRtUgY1sPK0zyONZshR+Yypt4a+GeUGrFQ355ay9MQJdZ/iDzKOSyi
        oapmSw==
        """

    private static let future = """
        MIIC7jCCAdagAwIBAgIBBDANBgkqhkiG9w0BAQsFADAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwHhcNMzAxMDAxMDAwMDAwWhcNMzAx
        MjMxMDAwMDAwWjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQCj8sikOTS+ZhcQ
        DFLG9kzMioPOta7Ght1gPUSRK9UBJlSHtPFlJFl2pwHKL8tKC8gNy0lB3MyWTjayaBZElgGXPgZTae56xrPRzkVpnNo4CrkEyZ0g
        gHI+VmQa7gbvCdM0AF/GILQxhxUD3o05HBwGDNLr7EAbk3AVy63L9Koc7qIevFsin9sTBk+aE+m8yihRJKYhMXBodjw8Lx98HJuz
        062OV15ynXk2s6SIhUpg2b6Kj3UqmMv2aoSzA8X7Rl+LOm+EWQCKBQRh+4d0V1Un/Rny7qqbj6930HVS298thB7JFHcxWecZmhsV
        jE9kAavET8o4qJJdPGxmiwluFqzVAgMBAAGjSzBJMAwGA1UdEwEB/wQCMAAwDgYDVR0PAQH/BAQDAgWgMBMGA1UdJQQMMAoGCCsG
        AQUFBwMBMBQGA1UdEQQNMAuCCWxvY2FsaG9zdDANBgkqhkiG9w0BAQsFAAOCAQEAgAwI8Mc2BG3ltX7BwGzq1E2h7pBvkhaMRCBy
        WECehwOo53V7yGA/qBGSHBuZ9NAaRG4VQoq1aRDcY68pBDlmHFX5EMpXY3LRqJ8NYt4vTR42HwW4Ij7LqBUbX9v1Q1uqM6rOtn/f
        M4xUkvGhgchykeZpeSDPx5mkizDYVuOkXLtcJDWCdMZvZctq9aGJjVRiGgedjFbU5Zmq8AFMpBPMl5sYxvlsq3ZPFGpGNkILncCf
        So5Y+MLXdL1ZRQJQQyS4AFw1+Au1IARCSl8E5s/qaaDbCmY8f4xL8vVPUKVApoLOpJSUJmxxVvULzCBw44xrxnyhOl/9PR/3IM2I
        Q/kXpg==
        """

    private static let clientOnly = """
        MIIC7jCCAdagAwIBAgIBBTANBgkqhkiG9w0BAQsFADAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwHhcNMjYxMDAxMDAwMDAwWhcNMjYx
        MjMxMDAwMDAwWjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQCj8sikOTS+ZhcQ
        DFLG9kzMioPOta7Ght1gPUSRK9UBJlSHtPFlJFl2pwHKL8tKC8gNy0lB3MyWTjayaBZElgGXPgZTae56xrPRzkVpnNo4CrkEyZ0g
        gHI+VmQa7gbvCdM0AF/GILQxhxUD3o05HBwGDNLr7EAbk3AVy63L9Koc7qIevFsin9sTBk+aE+m8yihRJKYhMXBodjw8Lx98HJuz
        062OV15ynXk2s6SIhUpg2b6Kj3UqmMv2aoSzA8X7Rl+LOm+EWQCKBQRh+4d0V1Un/Rny7qqbj6930HVS298thB7JFHcxWecZmhsV
        jE9kAavET8o4qJJdPGxmiwluFqzVAgMBAAGjSzBJMAwGA1UdEwEB/wQCMAAwDgYDVR0PAQH/BAQDAgWgMBMGA1UdJQQMMAoGCCsG
        AQUFBwMCMBQGA1UdEQQNMAuCCWxvY2FsaG9zdDANBgkqhkiG9w0BAQsFAAOCAQEAb/z6924z4bCDlbtGmuq3Qa5c8wG+d4scL2va
        fJ7VHBJ1zce80+rH8bsBqTw5XrgOMRfqzg6fZBbD2Gx5nVet/n8xO41bd0cqvSIfSCuKQaaWtgp98BicpNTOCdpF30CM26EIz9se
        quebUJste7aDfkrLJiPxby2Fv4EJ7+uuXxTJ8iKMt6ua/ySXier9bcMRhx0ILnLuOBIrnM6KSs1aXNAMteF/tWgonPqAVn8cq3C1
        fM7MTX42nC7XrpAJUF/Sk8bzGNjF6+pyO53acYD55mq07ScgbUHf+3WNIlaSm8Y/PBiXd0dZRH+NtxYRNQxflm/EUV4gns3a4xYh
        VqOGsg==
        """

    private static let publicRoot = """
        MIIC1DCCAbygAwIBAgIBBjANBgkqhkiG9w0BAQsFADAbMRkwFwYDVQQDDBBDaW5lbWEgdGVzdCByb290MB4XDTI2MTAwMTAwMDAw
        MFoXDTI2MTIzMTAwMDAwMFowGzEZMBcGA1UEAwwQQ2luZW1hIHRlc3Qgcm9vdDCCASIwDQYJKoZIhvcNAQEBBQADggEPADCCAQoC
        ggEBALm7AGF/ZomTo1lxWLhZGfZcM10awOMvklW6AXildJ5ueM5J3wUANb64Uc+7Yp6ySmqmHzfY8B2BzQxQuFjMO1g1Jinqkwdp
        Lipf3plDU7dy/OQJBBbr9owNjinlQ5O1X0QW+jbrOVCUmtldwzB1yvvO/yP4tXyveOFtEPluV4aiMBt7+fwi6llleJpd3muR29DC
        OEnXh1kbrJ8tamgZ8u7EpFXZpv6QuV1eE4Vy0/phWSG2EAQLQdPumerzZKUBtvEyIPeiClT7dIb4DZnaRl0JVhSmD/O2WCC+eIMK
        CVRTLmIEr+7+UQzkgo8eLwno4MP7WZslPKvCo0QmHbBcKrsCAwEAAaMjMCEwDwYDVR0TAQH/BAUwAwEB/zAOBgNVHQ8BAf8EBAMC
        AaYwDQYJKoZIhvcNAQELBQADggEBALfe6Ob05NP45voaRxNXBo7dhWm/aQCpW68jf0YjUmGM3mSyaKwaSYMsY2vUY4TFQ4MIySWJ
        rnURIBV2oI3egj6CkQqVcIBdTYskSQLJH7Hq4yGp/U4QbpdrSdKilM213o2fkAlJ1idzi2RfjBU1Yd77tTZ8pjrek1tBF3XPtOSU
        epKrs0oUtWoGqwNBaI/I6z48arOYg3XzAz9npKxHZ2ZD6kgmTQqApHFP5t5IyxrdMBqccXDvDUOzsJyrfk2bYh63We7gnlGIgD0K
        wUOQFp1EeYYYWGG31jaQeLCoVJs4EawSHKgwxLwrP1LthUdTvFqllMk2Hl50l+5ODLLKf38=
        """

    private static let publicLeaf = """
        MIIC9TCCAd2gAwIBAgIBBzANBgkqhkiG9w0BAQsFADAbMRkwFwYDVQQDDBBDaW5lbWEgdGVzdCByb290MB4XDTI2MTAwMTAwMDAw
        MFoXDTI2MTIzMTAwMDAwMFowFDESMBAGA1UEAwwJbG9jYWxob3N0MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAxsme
        SJR2l1HghCgoGSZ72sSvZuCJp2by2AakTkIYKwwm/co6DndDbSFh5d1pUu21wglWDQgqHv5MUvCJI0AwQ3D6zd0egaf+Of9SxAxw
        18SXJWfhh9hmfg++7JyWvQWo4BQYwLLntiOrvZWHZYjAoyaNL3VrAfJf32NHsnrHUgoOyox4miN+abJm+AntDeMRdfjz1+69W/PJ
        Q/izSoQiythJFhnh6ufp3rop+agost7CfOPyYSUrWE8Of4tyGQwA/kHwA6eK83uO4cMilg8sneObzUigSfazkWcZyFsK8F3UcZWi
        uibApBVsTw1Of7LDaHV60H2i7dptexmxTeUMcwIDAQABo0swSTAMBgNVHRMBAf8EAjAAMA4GA1UdDwEB/wQEAwIFoDATBgNVHSUE
        DDAKBggrBgEFBQcDATAUBgNVHREEDTALgglsb2NhbGhvc3QwDQYJKoZIhvcNAQELBQADggEBADAPzfKIeV1B3SChqW5lyLzaOmor
        XN+4zsrOX7z22RtEmVKcx+A7mmOZaD4/OJA+TSZUgfa8ow28aveBtuIN69f2SQDHn4d1k101IBnX2l0r0tkS68kvAA/OPy+fyeU0
        jPISxKcHiQ2NB+TsBrULqaMbM/tdDltjWz62JFxoznZSNcrHN3qgUBublJywnOs3X28R6HMfYYs4xoH2jjYQTjtCj9swQaU6C/e5
        vlar5EWcNcVk+9kp4p6LLXAvpoi16mwFOG5ku4mrbIzA85npg/ZkuRvcbxobt0/X4icgpq0J/PuFho9jASpoUjCczPRelkgUTR83
        v6Vb8A4Ij4JkfFk=
        """
}
