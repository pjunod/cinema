import Foundation

/// Receipt handling around an actual URLSession exchange. It neither creates
/// bearer authority nor changes the caller's HTTP/status/cancellation policy.
final class ReadAfterHTTP: @unchecked Sendable {
    struct Authorization {
        let origin: String
        let token: String?
        let generation: UInt64
        var peers: [String] = []
    }

    private let authorization: () -> Authorization
    private let receipts: ReadAfterReceipt
    private let recordPeers: ([String], Authorization) -> Void
    private static let authorizationKey = "tv.plurx.read-after.authorization"

    init(authorization: @escaping () -> Authorization, receipts: ReadAfterReceipt = ReadAfterReceipt(),
         recordPeers: @escaping ([String], Authorization) -> Void = { _, _ in }) {
        self.authorization = authorization
        self.receipts = receipts
        self.recordPeers = recordPeers
    }

    /// Local request metadata, never a header. Bind at the same instant as
    /// bearer authorization, before an async executor hop can change login.
    static func bind(_ authorization: Authorization, to request: inout URLRequest) {
        let mutable = (request as NSURLRequest).mutableCopy() as! NSMutableURLRequest
        URLProtocol.setProperty(authorization, forKey: authorizationKey, in: mutable)
        request = mutable as URLRequest
    }

    struct Exchange {
        let request: URLRequest
        fileprivate let captured: Authorization?
        fileprivate let ticket: ReadAfterReceipt.Request?
        fileprivate let mutating: Bool
    }

    /// Custom bounded-body delegates use the same transaction without giving
    /// up their body ceilings, cancellation or redirect policy.
    func prepare(for original: URLRequest) -> Exchange {
        let captured = URLProtocol.property(forKey: Self.authorizationKey, in: original) as? Authorization
        var request = original
        // Only this adapter's current, proven floor may leave the client.
        request.setValue(nil, forHTTPHeaderField: "X-Plurx-Read-After")
        let ticket: ReadAfterReceipt.Request?
        if let captured, let token = captured.token, !token.isEmpty,
           request.value(forHTTPHeaderField: "Authorization") == "Bearer \(token)",
           let url = request.url, Self.isAccountAPI(url, authorization: captured),
           Self.isAccountAPI(url, authorization: authorization()) {
            ticket = receipts.request(generation: captured.generation,
                                      currentGeneration: { self.authorization().generation })
        } else {
            ticket = nil
        }
        if let index = ticket?.index { request.setValue(index, forHTTPHeaderField: "X-Plurx-Read-After") }
        let method = request.httpMethod?.uppercased() ?? "GET"
        let mutating = method != "GET" && method != "HEAD"
        return Exchange(request: request, captured: captured, ticket: ticket, mutating: mutating)
    }

    func failed(_ exchange: Exchange) {
        if let ticket = exchange.ticket {
            receipts.transportFailed(mutating: exchange.mutating, request: ticket,
                                     currentGeneration: { self.authorization().generation })
        }
    }

    func received(_ response: URLResponse, data: Data = Data(), exchange: Exchange, redirected: Bool = false) {
        let original = exchange.request
        if let ticket = exchange.ticket {
            // A redirected answer cannot attest the original write. The
            // delegate stripped its receipt before any redirected request.
            if redirected || response.url != original.url {
                failed(exchange)
            } else {
                if original.httpMethod?.uppercased() == "GET", original.url?.path == "/api/v1/cluster/ingress",
                   let captured = exchange.captured, let response = response as? HTTPURLResponse,
                   (200..<300).contains(response.statusCode), data.count <= 1_048_576,
                   let ingress = try? JSONDecoder().decode(Ingress.self, from: data) {
                    recordPeers(ingress.node_urls, captured)
                }
                receipts.observe((response as? HTTPURLResponse)?.value(forHTTPHeaderField: "X-Plurx-Commit-Index"),
                                 mutating: exchange.mutating, request: ticket,
                                 currentGeneration: { self.authorization().generation })
            }
        }
    }

    func data(for original: URLRequest, using session: URLSession, refuseRedirects: Bool = false) async throws -> (Data, URLResponse) {
        let exchange = prepare(for: original)
        let redirect = ReadAfterRedirect(refuse: refuseRedirects)
        let result: (Data, URLResponse)
        do { result = try await session.data(for: exchange.request, delegate: redirect) }
        catch { failed(exchange); throw error }
        received(result.1, data: result.0, exchange: exchange, redirected: redirect.wasRedirected)
        return result
    }

    /// Stream caption pages and immutable segments under their byte ceiling.
    func boundedData(for original: URLRequest, using session: URLSession, maximumBytes: Int) async throws -> (Data, URLResponse) {
        guard (1...65_536).contains(maximumBytes) else { throw CaptionProtocolError.budget }
        let exchange = prepare(for: original)
        let redirect = ReadAfterRedirect(refuse: true)
        do {
            let (bytes, response) = try await session.bytes(for: exchange.request, delegate: redirect)
            guard response.expectedContentLength <= Int64(maximumBytes) else {
                bytes.task.cancel()
                throw CaptionProtocolError.budget
            }
            var data = Data()
            for try await byte in bytes {
                if data.count >= maximumBytes { bytes.task.cancel(); throw CaptionProtocolError.budget }
                data.append(byte)
            }
            received(response, data: data, exchange: exchange, redirected: redirect.wasRedirected)
            return (data, response)
        } catch { failed(exchange); throw error }
    }

    private struct Ingress: Decodable { let node_urls: [String] }

    private static func isAccountAPI(_ url: URL, authorization: Authorization) -> Bool {
        ([authorization.origin] + authorization.peers).contains { isAccountAPI(url, origin: $0) }
    }

    private static func isAccountAPI(_ url: URL, origin: String) -> Bool {
        guard let canonical = Session.canonicalOrigin(origin),
              let primary = URLComponents(string: canonical),
              let target = URLComponents(url: url, resolvingAgainstBaseURL: false),
              let scheme = primary.scheme?.lowercased(), scheme == "http" || scheme == "https",
              primary.user == nil, primary.password == nil,
              primary.query == nil, primary.fragment == nil,
              primary.path.isEmpty || primary.path == "/",
              target.user == nil, target.password == nil,
              target.scheme?.lowercased() == scheme,
              let host = primary.host?.lowercased(), target.host?.lowercased() == host,
              (primary.port ?? (scheme == "https" ? 443 : 80)) ==
                  (target.port ?? (scheme == "https" ? 443 : 80))
        else { return false }
        return target.path.hasPrefix("/api/v1/") &&
            target.path.split(separator: "/").dropFirst(2).first != "server"
    }
}

/// URLSession ordinarily forwards custom headers on redirects. Never forward
/// this per-account floor, even if the destination happens to share a host.
private final class ReadAfterRedirect: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    private let refuse: Bool
    init(refuse: Bool) { self.refuse = refuse; super.init() }
    private let lock = NSLock()
    private var redirected = false
    var wasRedirected: Bool { lock.lock(); defer { lock.unlock() }; return redirected }

    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest,
                    completionHandler: @escaping (URLRequest?) -> Void) {
        lock.lock(); redirected = true; lock.unlock()
        var redirectedRequest = request
        redirectedRequest.setValue(nil, forHTTPHeaderField: "X-Plurx-Read-After")
        completionHandler(refuse ? nil : redirectedRequest)
    }
}
