import AVFoundation
import CryptoKit
import Foundation
import Security

/// Trust on first use for an explicitly selected Cinema server. A private
/// identity is remembered per HTTPS origin; it is never a global TLS exception.
/// Publicly trusted servers keep the system's normal certificate validation.
final class ServerTrust: NSObject, URLSessionTaskDelegate, AVAssetResourceLoaderDelegate, @unchecked Sendable {
    static let shared = ServerTrust()
    private let lock = NSLock()
    private var selectedHosts = Set<String>()
    private var privateHosts = Set<String>()
    private var changedIdentities: [String: String] = [:]
    private let vault: (String) -> any TokenStoring
    private let now: () -> Date
    private let mediaQueue = DispatchQueue(label: "tv.plurx.server-trust")

    init(vault: @escaping (String) -> any TokenStoring = {
        TokenVault(service: "tv.plurx.server-trust", account: $0)
    }, now: @escaping () -> Date = Date.init) {
        self.vault = vault
        self.now = now
        super.init()
    }

    /// HTTP discovery may redirect to the same host's configured HTTPS port.
    /// Only selected API hosts and their authenticated cluster peers enroll;
    /// arbitrary image/media URLs cannot enroll a new private identity.
    static func allowFirstUse(origin: String) { shared.allowFirstUse(origin: origin) }

    func allowFirstUse(origin: String) {
        guard let url = URL(string: origin),
              ["http", "https"].contains(url.scheme?.lowercased() ?? ""),
              url.user == nil, url.password == nil,
              let host = url.host?.lowercased(), !host.isEmpty else { return }
        lock.lock(); defer { lock.unlock() }
        selectedHosts.insert(host)
    }

    func changedOrigin(for origin: String) -> String? {
        guard let host = URL(string: origin)?.host?.lowercased() else { return nil }
        lock.lock(); defer { lock.unlock() }
        return changedIdentities.keys.sorted().first { URL(string: $0)?.host?.lowercased() == host }
    }

    func failureMessage(for origin: String) -> String? {
        guard changedOrigin(for: origin) != nil else { return nil }
        return "This server's certificate has changed. Verify that you are connecting to your server before trusting its replacement identity."
    }

    /// An explicit recovery action, after discarding the previous login. A
    /// routine retry, sign-out or new API object never replaces a stored pin.
    @discardableResult
    func forgetChangedIdentity(origin: String) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard let remembered = changedIdentities[origin] else { return false }
        let storage = vault(origin)
        guard storage.read() == remembered else { return false }
        storage.clear()
        guard storage.read() == nil else { return false }
        changedIdentities.removeValue(forKey: origin)
        return true
    }

    static var session: URLSession { defaultSession }
    private static let defaultSession = session(configuration: .default)

    static func session(configuration: URLSessionConfiguration) -> URLSession {
        URLSession(configuration: configuration, delegate: shared, delegateQueue: nil)
    }

    static func asset(url: URL) -> AVURLAsset {
        // AVFoundation's HLS fragment loader does not consult the resource
        // loader's server-trust delegate. Fetch remote media through the same
        // pinned transport as the API, behind an app-private loopback bridge.
        let playbackURL = shared.requiresMediaProxy(for: url)
            ? (PlayerTrustProxy.shared.url(for: url) ?? url) : url
        let asset = AVURLAsset(url: playbackURL)
        asset.resourceLoader.setDelegate(shared, queue: shared.mediaQueue)
        return asset
    }

    static func playerItem(url: URL) -> AVPlayerItem { AVPlayerItem(asset: asset(url: url)) }

    func requiresMediaProxy(for url: URL) -> Bool {
        guard let host = url.host?.lowercased() else { return false }
        lock.lock(); defer { lock.unlock() }
        if privateHosts.contains(host) { return true }
        guard url.scheme?.lowercased() == "https" else { return false }
        let origin = "https://\(host.contains(":") ? "[\(host)]" : host):\(url.port ?? 443)"
        return vault(origin).read() != nil
    }

    func urlSession(_ session: URLSession, didReceive challenge: URLAuthenticationChallenge,
                    completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        let result = disposition(for: challenge)
        completionHandler(result.0, result.1)
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didReceive challenge: URLAuthenticationChallenge,
                    completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        let result = disposition(for: challenge)
        completionHandler(result.0, result.1)
    }

    func resourceLoader(_ resourceLoader: AVAssetResourceLoader,
                        shouldWaitForResponseTo authenticationChallenge: URLAuthenticationChallenge) -> Bool {
        guard authenticationChallenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust else {
            return false
        }
        let result = disposition(for: authenticationChallenge)
        switch result.0 {
        case .useCredential:
            if let credential = result.1 {
                authenticationChallenge.sender?.use(credential, for: authenticationChallenge)
            } else {
                authenticationChallenge.sender?.cancel(authenticationChallenge)
            }
        case .performDefaultHandling:
            authenticationChallenge.sender?.performDefaultHandling?(for: authenticationChallenge)
        default:
            authenticationChallenge.sender?.cancel(authenticationChallenge)
        }
        return true
    }

    func disposition(for challenge: URLAuthenticationChallenge) -> (URLSession.AuthChallengeDisposition, URLCredential?) {
        let space = challenge.protectionSpace
        guard space.authenticationMethod == NSURLAuthenticationMethodServerTrust else {
            return (.performDefaultHandling, nil)
        }
        guard space.protocol?.lowercased() == "https", let trust = space.serverTrust,
              let certificates = SecTrustCopyCertificateChain(trust) as? [SecCertificate],
              let leaf = certificates.first else { return (.cancelAuthenticationChallenge, nil) }
        let host = space.host.lowercased()
        let origin = "https://\(host.contains(":") ? "[\(host)]" : host):\(space.port)"
        let fingerprint = Data(SHA256.hash(data: SecCertificateCopyData(leaf) as Data)).base64EncodedString()

        // Serialize enrollment with comparison and durable publication, so two
        // racing first connections cannot replace one another's identity.
        lock.lock(); defer { lock.unlock() }
        let storage = vault(origin)
        let remembered = storage.read()
        if let remembered, remembered != fingerprint {
            changedIdentities[origin] = remembered
            return (.cancelAuthenticationChallenge, nil)
        }
        if remembered == nil && SecTrustEvaluateWithError(trust, nil) {
            return (.performDefaultHandling, nil)
        }
        guard remembered != nil || selectedHosts.contains(host), certificates.count == 1,
              let issuer = SecCertificateCopyNormalizedIssuerSequence(leaf),
              let subject = SecCertificateCopyNormalizedSubjectSequence(leaf), issuer == subject else {
            return (.cancelAuthenticationChallenge, nil)
        }

        // A pinned private identity authenticates the endpoint independently of
        // DNS names (including existing localhost-only generated certificates).
        // Keep TLS server purpose, key-strength and validity checks. Build a new
        // trust instead of changing the system evaluation's policies/anchors.
        var privateTrust: SecTrust?
        guard SecTrustCreateWithCertificates([leaf] as CFArray, SecPolicyCreateSSL(true, nil), &privateTrust) == errSecSuccess,
              let privateTrust,
              SecTrustSetAnchorCertificates(privateTrust, [leaf] as CFArray) == errSecSuccess,
              SecTrustSetAnchorCertificatesOnly(privateTrust, true) == errSecSuccess,
              SecTrustSetNetworkFetchAllowed(privateTrust, false) == errSecSuccess,
              SecTrustSetVerifyDate(privateTrust, now() as CFDate) == errSecSuccess,
              SecTrustEvaluateWithError(privateTrust, nil),
              remembered != nil || storage.write(fingerprint) else {
            return (.cancelAuthenticationChallenge, nil)
        }
        changedIdentities.removeValue(forKey: origin)
        privateHosts.insert(host)
        return (.useCredential, URLCredential(trust: privateTrust))
    }
}
