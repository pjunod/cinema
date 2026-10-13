import Foundation
import Network
import Darwin

/// One immutable archive, one foreground lifetime and one download. The
/// capability is unrelated to playback, support enrollment or filesystem paths.
final class DiagnosticLocalExport: @unchecked Sendable {
    enum Failure: LocalizedError {
        case network, invalid
        var errorDescription: String? { "Connect the TV and phone to the same local network, then retry." }
    }
    private let queue = DispatchQueue(label: "cinema.report.download")
    private let listener: NWListener
    private var peers: [ObjectIdentifier: NWConnection] = [:]
    private var headerTimers: [ObjectIdentifier: DispatchWorkItem] = [:]
    private var archive: Data
    private var live = true
    private var requests = 0
    private var ready: CheckedContinuation<URL, Error>?
    private var started = false
    private let token = UUID().uuidString.replacingOccurrences(of: "-", with: "") + UUID().uuidString.replacingOccurrences(of: "-", with: "")
    private let filename: String
    private let host: String
    private let ended: @Sendable () -> Void
    private var expiry: DispatchWorkItem?
    init(bytes: Data, reportID: String, host: String? = nil, lifetime: TimeInterval = 300, ended: @escaping @Sendable () -> Void = {}) throws {
        guard !bytes.isEmpty, bytes.count <= 8_388_608, UUID(uuidString: reportID) != nil, lifetime > 0, lifetime <= 300 else { throw Failure.invalid }
        self.archive = bytes; self.filename = "cinema-report-" + reportID + ".zip"; self.ended = ended
        self.host = try host ?? Self.localAddress()
        let parameters = NWParameters.tcp
        parameters.requiredLocalEndpoint = .hostPort(host: NWEndpoint.Host(self.host), port: .any)
        listener = try NWListener(using: parameters)
        let timer = DispatchWorkItem { [weak self] in self?.finish() }
        expiry = timer; queue.asyncAfter(deadline: .now() + lifetime, execute: timer)
    }
    func start() async throws -> URL {
        try await withCheckedThrowingContinuation { continuation in
            queue.async { [self] in
                guard live, !started else { continuation.resume(throwing: Failure.network); return }
                started = true; ready = continuation
                listener.stateUpdateHandler = { [weak self] state in
                    guard let self else { return }
                    switch state {
                    case .ready:
                        guard let pending = ready, live, let port = listener.port, let url = URL(string: "http://\(host):\(port.rawValue)/\(token)") else { return }
                        ready = nil; pending.resume(returning: url)
                    case .failed:
                        finish()
                    case .cancelled:
                        if live { finish() }
                    default: break
                    }
                }
                listener.newConnectionHandler = { [weak self] peer in
                    guard let self, live, peers.isEmpty, requests < 32 else { peer.cancel(); return }
                    requests += 1; peers[ObjectIdentifier(peer)] = peer; peer.start(queue: queue)
                    let timer = DispatchWorkItem { [weak self, weak peer] in
                        guard let peer else { return }; self?.drop(peer)
                    }
                    headerTimers[ObjectIdentifier(peer)] = timer
                    queue.asyncAfter(deadline: .now() + 2, execute: timer)
                    receive(peer, header: Data())
                }
                listener.start(queue: queue)
            }
        }
    }
    func stop() { queue.async { [self] in finish() } }
    private func drop(_ peer: NWConnection) { headerTimers.removeValue(forKey: ObjectIdentifier(peer))?.cancel(); peers.removeValue(forKey: ObjectIdentifier(peer)); peer.cancel() }
    private func finish() {
        guard live || !archive.isEmpty else { return }
        live = false; if let pending = ready { ready = nil; pending.resume(throwing: Failure.network) }
        expiry?.cancel(); expiry = nil; listener.cancel()
        headerTimers.values.forEach { $0.cancel() }; headerTimers.removeAll(); peers.values.forEach { $0.cancel() }; peers.removeAll(); archive = Data(); listener.stateUpdateHandler = nil; listener.newConnectionHandler = nil; ended()
    }
    private func receive(_ peer: NWConnection, header: Data) {
        peer.receive(minimumIncompleteLength: 1, maximumLength: 8192 - header.count) { [weak self] data, _, done, error in
            guard let self, live, error == nil else { peer.cancel(); return }
            var next = header; if let data { next.append(data) }
            guard next.count < 8192 else { drop(peer); return }
            if next.range(of: Data("\r\n\r\n".utf8)) != nil { respond(peer, header: next) }
            else if done { drop(peer) }
            else { receive(peer, header: next) }
        }
    }
    private func respond(_ peer: NWConnection, header: Data) {
        headerTimers.removeValue(forKey: ObjectIdentifier(peer))?.cancel()
        let lines = String(decoding: header, as: UTF8.self).components(separatedBy: "\r\n")
        let request = (lines.first ?? "").components(separatedBy: " ")
        guard request.count == 3, request[1] == "/" + token, ["HTTP/1.0", "HTTP/1.1"].contains(request[2]) else { reply(peer, status: 404, type: "text/plain", body: Data()); return }
        var fields: [String: String] = [:]
        for line in lines.dropFirst() where !line.isEmpty {
            guard let colon = line.firstIndex(of: ":") else { drop(peer); return }
            let key = line[..<colon].lowercased(); let value = String(line[line.index(after: colon)...]).trimmingCharacters(in: .whitespaces)
            guard fields[key] == nil else { drop(peer); return }; fields[key] = value
        }
        guard fields["transfer-encoding"] == nil, fields["content-length"] == nil || fields["content-length"] == "0" else { drop(peer); return }
        if request[0] == "GET" {
            let page = "<!doctype html><meta name=viewport content='width=device-width'><title>Cinema diagnostic ZIP</title><h1>Save this TV’s diagnostic ZIP</h1><p>Attach the downloaded ZIP to your bug or issue report. This does not submit it.</p><form method=post><button>Download diagnostic ZIP</button></form>"
            reply(peer, status: 200, type: "text/html; charset=utf-8", body: Data(page.utf8)); return
        }
        let origin = "http://\(host):\(listener.port?.rawValue ?? 0)"
        guard request[0] == "POST", fields["origin"] == nil || fields["origin"] == origin, fields["sec-fetch-site"] != "cross-site" else { reply(peer, status: 403, type: "text/plain", body: Data()); return }
        live = false; listener.cancel()
        reply(peer, status: 200, type: "application/zip", body: archive, attachment: filename)
    }
    private func reply(_ peer: NWConnection, status: Int, type: String, body: Data, attachment: String? = nil) {
        var header = "HTTP/1.1 \(status) \(status == 200 ? "OK" : "Rejected")\r\nContent-Type: \(type)\r\nContent-Length: \(body.count)\r\nConnection: close\r\nCache-Control: no-store\r\nReferrer-Policy: same-origin\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'; form-action 'self'; frame-ancestors 'none'\r\n"
        if let attachment { header += "Content-Disposition: attachment; filename=\"\(attachment)\"\r\n" }
        var response = Data((header + "\r\n").utf8); response.append(body)
        peer.send(content: response, completion: .contentProcessed { [weak self] _ in
            guard let self else { return }; drop(peer); if !live { finish() }
        })
    }
    private static func localAddress() throws -> String {
        var list: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&list) == 0 else { throw Failure.network }; defer { freeifaddrs(list) }
        var entry = list
        while let current = entry {
            defer { entry = current.pointee.ifa_next }
            let name = String(cString: current.pointee.ifa_name)
            guard name.hasPrefix("en"), let address = current.pointee.ifa_addr, address.pointee.sa_family == UInt8(AF_INET) else { continue }
            var buffer = [CChar](repeating: 0, count: Int(NI_MAXHOST))
            guard getnameinfo(address, socklen_t(address.pointee.sa_len), &buffer, socklen_t(buffer.count), nil, 0, NI_NUMERICHOST) == 0 else { continue }
            let host = String(cString: buffer); let octets = host.split(separator: ".").compactMap { Int($0) }
            if octets.count == 4, octets[0] == 10 || (octets[0] == 172 && (16...31).contains(octets[1])) || (octets[0] == 192 && octets[1] == 168) { return host }
        }
        throw Failure.network
    }
}
