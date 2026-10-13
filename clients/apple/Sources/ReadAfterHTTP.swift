import Foundation

/// Receipt handling around an actual URLSession exchange. It neither creates
/// bearer authority nor changes the caller's HTTP/status/cancellation policy.
final class ReadAfterHTTP: @unchecked Sendable {
    struct Authorization {
        let origin: String
        let token: String?
        let generation: UInt64
    }

    private let authorization: () -> Authorization
    private let receipts: ReadAfterReceipt

    init(authorization: @escaping () -> Authorization, receipts: ReadAfterReceipt = ReadAfterReceipt()) {
        self.authorization = authorization
        self.receipts = receipts
    }

    func data(for original: URLRequest, using session: URLSession) async throws -> (Data, URLResponse) {
        let captured = authorization()
        var request = original
        // Only this adapter's current, proven floor may leave the client.
        request.setValue(nil, forHTTPHeaderField: "X-Plurx-Read-After")
        let ticket: ReadAfterReceipt.Request?
        if let token = captured.token, !token.isEmpty,
           request.value(forHTTPHeaderField: "Authorization") == "Bearer \(token)",
           let url = request.url, Self.isAccountAPI(url, origin: captured.origin) {
            ticket = receipts.request(generation: captured.generation,
                                      currentGeneration: { self.authorization().generation })
        } else {
            ticket = nil
        }
        if let index = ticket?.index { request.setValue(index, forHTTPHeaderField: "X-Plurx-Read-After") }
        let method = request.httpMethod?.uppercased() ?? "GET"
        let mutating = method != "GET" && method != "HEAD"
        let redirect = ReadAfterRedirect()
        let result: (Data, URLResponse)
        do { result = try await session.data(for: request, delegate: redirect) }
        catch {
            if let ticket {
                receipts.transportFailed(mutating: mutating, request: ticket,
                                         currentGeneration: { self.authorization().generation })
            }
            throw error
        }
        if let ticket {
            // A redirected answer cannot attest the original write. The
            // delegate stripped its receipt before any redirected request.
            if redirect.wasRedirected || result.1.url != original.url {
                receipts.transportFailed(mutating: mutating, request: ticket,
                                         currentGeneration: { self.authorization().generation })
            } else {
                receipts.observe((result.1 as? HTTPURLResponse)?.value(forHTTPHeaderField: "X-Plurx-Commit-Index"),
                                 mutating: mutating, request: ticket,
                                 currentGeneration: { self.authorization().generation })
            }
        }
        return result
    }

    private static func isAccountAPI(_ url: URL, origin: String) -> Bool {
        guard let primary = URLComponents(string: origin),
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
    private let lock = NSLock()
    private var redirected = false
    var wasRedirected: Bool { lock.lock(); defer { lock.unlock() }; return redirected }

    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest,
                    completionHandler: @escaping (URLRequest?) -> Void) {
        lock.lock(); redirected = true; lock.unlock()
        var redirectedRequest = request
        redirectedRequest.setValue(nil, forHTTPHeaderField: "X-Plurx-Read-After")
        completionHandler(redirectedRequest)
    }
}
