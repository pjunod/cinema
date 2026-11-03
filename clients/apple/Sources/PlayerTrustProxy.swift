import CryptoKit
import Foundation
import Network

/// AVFoundation's HLS segment loader does not consistently surface server-trust
/// challenges to the asset resource loader. Keep that loader on app-local HTTP;
/// every upstream request still passes through the server's pinned TLS policy.
final class PlayerTrustProxy: @unchecked Sendable {
    static let shared = PlayerTrustProxy()
    private let queue = DispatchQueue(label: "tv.plurx.player-trust-proxy")
    private let capability = UUID().uuidString + UUID().uuidString
    private let encryptionKey = SymmetricKey(size: .bits256)
    private let mappingLock = NSLock()
    private var mappings: [String: String] = [:]
    private var mappingOrder: [String] = []
    private let ready = DispatchSemaphore(value: 0)
    private var listener: NWListener?
    private var port: UInt16?
    private var peers: [ObjectIdentifier: NWConnection] = [:]
    private var transfers: [ObjectIdentifier: Task<Void, Never>] = [:]
    private var timers: [ObjectIdentifier: DispatchWorkItem] = [:]
    private let transport = Transport()

    private init() {
        let parameters = NWParameters.tcp
        parameters.requiredLocalEndpoint = .hostPort(host: "127.0.0.1", port: .any)
        do {
            let listener = try NWListener(using: parameters)
            self.listener = listener
            listener.stateUpdateHandler = { [weak self] state in
                guard let self else { return }
                switch state {
                case .ready: self.port = listener.port?.rawValue; self.ready.signal()
                case .failed, .cancelled: self.ready.signal()
                default: break
                }
            }
            listener.newConnectionHandler = { [weak self] peer in self?.accept(peer) }
            listener.start(queue: queue)
            if ready.wait(timeout: .now() + 2) != .success {
                listener.cancel()
            }
        } catch { listener = nil }
    }

    func url(for upstream: URL) -> URL? {
        guard let port, Self.remoteURL(upstream) else { return nil }
        if originalURL(for: upstream) != nil { return upstream }
        let identity = upstream.absoluteString
        guard identity.utf8.count <= 8192 else { return nil }
        mappingLock.lock(); defer { mappingLock.unlock() }
        let encoded: String
        if let existing = mappings[identity] {
            encoded = existing
        } else {
            // A diagnostic may contain a local media URL. Keep its embedded
            // upstream credentials confidential as well as capability protected.
            guard let sealed = try? AES.GCM.seal(Data(identity.utf8), using: encryptionKey),
                  let combined = sealed.combined else { return nil }
            encoded = combined.base64EncodedString()
                .replacingOccurrences(of: "+", with: "-")
                .replacingOccurrences(of: "/", with: "_")
                .replacingOccurrences(of: "=", with: "")
            // Stable URLs matter when a live playlist repeats recent segments.
            // Old ciphertext remains decryptable after this bounded cache evicts.
            if mappingOrder.count == 8192 {
                mappings.removeValue(forKey: mappingOrder.removeFirst())
            }
            mappings[identity] = encoded
            mappingOrder.append(identity)
        }
        return URL(string: "http://127.0.0.1:\(port)/\(capability)/\(encoded)")
    }

    func originalURL(for local: URL) -> URL? {
        guard let port, local.scheme == "http", local.host == "127.0.0.1", local.port == Int(port),
              local.user == nil, local.password == nil, local.query == nil, local.fragment == nil else { return nil }
        return upstream(local.path)
    }

    private static func remoteURL(_ url: URL) -> Bool {
        ["https", "http"].contains(url.scheme?.lowercased() ?? "") &&
            url.host?.isEmpty == false && url.user == nil && url.password == nil
    }

    private func upstream(_ path: String) -> URL? {
        let prefix = "/" + capability + "/"
        guard path.hasPrefix(prefix), !path.contains("?"), path.utf8.count < 16_384 else { return nil }
        var encoded = String(path.dropFirst(prefix.count))
            .replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        encoded += String(repeating: "=", count: (4 - encoded.count % 4) % 4)
        guard let bytes = Data(base64Encoded: encoded),
              let sealed = try? AES.GCM.SealedBox(combined: bytes),
              let plaintext = try? AES.GCM.open(sealed, using: encryptionKey),
              let text = String(data: plaintext, encoding: .utf8),
              let url = URL(string: text), Self.remoteURL(url) else { return nil }
        return url
    }

    private func accept(_ peer: NWConnection) {
        guard peers.count < 32 else { peer.cancel(); return }
        let key = ObjectIdentifier(peer)
        peers[key] = peer
        peer.stateUpdateHandler = { [weak self, weak peer] state in
            guard let self, let peer else { return }
            if case .failed = state { self.drop(peer) }
            if case .cancelled = state { self.drop(peer) }
        }
        let timer = DispatchWorkItem { [weak self, weak peer] in
            guard let self, let peer else { return }; self.drop(peer)
        }
        timers[key] = timer
        queue.asyncAfter(deadline: .now() + 10, execute: timer)
        peer.start(queue: queue)
        receive(peer, header: Data())
    }

    private func drop(_ peer: NWConnection) {
        let key = ObjectIdentifier(peer)
        timers.removeValue(forKey: key)?.cancel()
        transfers.removeValue(forKey: key)?.cancel()
        peers.removeValue(forKey: key)
        peer.stateUpdateHandler = nil
        peer.cancel()
    }

    private func receive(_ peer: NWConnection, header: Data) {
        peer.receive(minimumIncompleteLength: 1, maximumLength: 16_384 - header.count) { [weak self] data, _, done, error in
            guard let self, error == nil else { peer.cancel(); return }
            var next = header
            if let data { next.append(data) }
            guard next.count < 16_384 else { self.drop(peer); return }
            if next.range(of: Data("\r\n\r\n".utf8)) != nil { self.respond(peer, header: next) }
            else if done { self.drop(peer) }
            else { self.receive(peer, header: next) }
        }
    }

    private func respond(_ peer: NWConnection, header: Data) {
        let key = ObjectIdentifier(peer)
        timers.removeValue(forKey: key)?.cancel()
        let lines = String(decoding: header, as: UTF8.self).components(separatedBy: "\r\n")
        let request = (lines.first ?? "").split(separator: " ").map(String.init)
        guard request.count == 3, ["GET", "HEAD"].contains(request[0]),
              ["HTTP/1.0", "HTTP/1.1"].contains(request[2]), let upstream = upstream(request[1]) else {
            reject(peer); return
        }
        var fields: [String: String] = [:]
        for line in lines.dropFirst() {
            if line.isEmpty { break }
            guard let colon = line.firstIndex(of: ":") else { reject(peer); return }
            let name = line[..<colon].lowercased()
            let value = String(line[line.index(after: colon)...]).trimmingCharacters(in: .whitespaces)
            guard fields[name] == nil else { reject(peer); return }
            fields[name] = value
        }
        guard fields["host"] == port.map({ "127.0.0.1:\($0)" }),
              fields["transfer-encoding"] == nil,
              fields["content-length"] == nil || fields["content-length"] == "0" else { reject(peer); return }
        var upstreamRequest = URLRequest(url: upstream, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 60)
        upstreamRequest.httpMethod = request[0]
        upstreamRequest.setValue("identity", forHTTPHeaderField: "Accept-Encoding")
        for field in ["range", "if-range"] {
            if let value = fields[field] { upstreamRequest.setValue(value, forHTTPHeaderField: field) }
        }
        transfers[key] = Task { [weak self, weak peer] in
            guard let self, let peer else { return }
            do { try await self.transfer(upstreamRequest, to: peer) }
            catch { /* Closing an incomplete response makes AVFoundation retry or fail. */ }
            self.queue.async { self.drop(peer) }
        }
    }

    private func reject(_ peer: NWConnection) {
        peer.send(content: Data("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".utf8), completion: .contentProcessed { [weak self] _ in
            guard let self else { return }
            self.queue.async { self.drop(peer) }
        })
    }

    private func transfer(_ request: URLRequest, to peer: NWConnection) async throws {
        let (bytes, response) = try await transport.session.bytes(for: request)
        guard let response = response as? HTTPURLResponse, let actual = response.url else { throw URLError(.badServerResponse) }
        let type = response.value(forHTTPHeaderField: "Content-Type") ?? "application/octet-stream"
        let playlist = actual.pathExtension.lowercased() == "m3u8" || type.lowercased().contains("mpegurl")
        if playlist && request.httpMethod != "HEAD" && response.statusCode == 200 {
            var body = Data()
            for try await byte in bytes {
                try Task.checkCancellation()
                guard body.count < 2_097_152 else { throw URLError(.dataLengthExceedsMaximum) }
                body.append(byte)
            }
            guard let source = String(data: body, encoding: .utf8), source.hasPrefix("#EXTM3U") else { throw URLError(.cannotDecodeContentData) }
            let rewritten = try Self.rewrite(source, relativeTo: actual) { remote in
                guard let mapped = self.url(for: remote) else { throw URLError(.badURL) }
                return mapped
            }
            let data = Data(rewritten.utf8)
            try await send(Self.header(response, length: data.count, playlist: true), to: peer)
            try await send(data, to: peer)
            return
        }
        try await send(Self.header(response, length: nil, playlist: false), to: peer)
        guard request.httpMethod != "HEAD" else { return }
        var chunk = Data(); chunk.reserveCapacity(65_536)
        for try await byte in bytes {
            try Task.checkCancellation()
            chunk.append(byte)
            if chunk.count == 65_536 { try await send(chunk, to: peer); chunk.removeAll(keepingCapacity: true) }
        }
        if !chunk.isEmpty { try await send(chunk, to: peer) }
    }

    static func rewrite(_ text: String, relativeTo base: URL, map: (URL) throws -> URL) throws -> String {
        let uri = try NSRegularExpression(pattern: #"URI="([^"]*)""#)
        return try text.components(separatedBy: .newlines).map { line in
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.isEmpty { return line }
            if !trimmed.hasPrefix("#") {
                guard let resolved = URL(string: trimmed, relativeTo: base)?.absoluteURL, remoteURL(resolved) else { throw URLError(.badURL) }
                guard base.scheme != "https" || resolved.scheme == "https" else { throw URLError(.secureConnectionFailed) }
                return try map(resolved).absoluteString
            }
            var rewritten = line
            for match in uri.matches(in: line, range: NSRange(line.startIndex..., in: line)).reversed() {
                guard let range = Range(match.range(at: 1), in: rewritten),
                      let resolved = URL(string: String(rewritten[range]), relativeTo: base)?.absoluteURL else { throw URLError(.badURL) }
                if remoteURL(resolved) {
                    guard base.scheme != "https" || resolved.scheme == "https" else { throw URLError(.secureConnectionFailed) }
                    rewritten.replaceSubrange(range, with: try map(resolved).absoluteString)
                } else if resolved.scheme?.lowercased() != "skd" {
                    throw URLError(.badURL)
                }
            }
            return rewritten
        }.joined(separator: "\n")
    }

    private static func header(_ response: HTTPURLResponse, length: Int?, playlist: Bool) -> Data {
        var result = "HTTP/1.1 \(response.statusCode) Response\r\nConnection: close\r\nCache-Control: no-store\r\n"
        let fields = playlist ? ["Content-Type"] : ["Content-Type", "Content-Length", "Content-Range", "Accept-Ranges", "ETag", "Last-Modified"]
        for field in fields {
            if field == "Content-Length", length != nil { continue }
            if let value = response.value(forHTTPHeaderField: field), !value.contains("\r"), !value.contains("\n") {
                result += "\(field): \(value)\r\n"
            }
        }
        if let length { result += "Content-Length: \(length)\r\n" }
        return Data((result + "\r\n").utf8)
    }

    private func send(_ data: Data, to peer: NWConnection) async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            peer.send(content: data, completion: .contentProcessed { error in
                if let error { continuation.resume(throwing: error) }
                else { continuation.resume() }
            })
        }
    }

    private final class Transport: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
        private(set) var session: URLSession!
        override init() {
            super.init()
            let configuration = URLSessionConfiguration.ephemeral
            configuration.httpCookieStorage = nil
            configuration.urlCredentialStorage = nil
            configuration.urlCache = nil
            configuration.timeoutIntervalForRequest = 60
            session = URLSession(configuration: configuration, delegate: self, delegateQueue: nil)
        }
        func urlSession(_ session: URLSession, didReceive challenge: URLAuthenticationChallenge,
                        completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
            let response = ServerTrust.shared.disposition(for: challenge)
            completionHandler(response.0, response.1)
        }
        func urlSession(_ session: URLSession, task: URLSessionTask, didReceive challenge: URLAuthenticationChallenge,
                        completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
            let response = ServerTrust.shared.disposition(for: challenge)
            completionHandler(response.0, response.1)
        }
        func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                        newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) {
            // TLS URLs never acquire a plaintext upstream through a redirect.
            completionHandler(response.url?.scheme == "https" && request.url?.scheme != "https" ? nil : request)
        }
    }
}
