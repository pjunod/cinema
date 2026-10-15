#if os(tvOS)
import Network
import Foundation

final class TVNavigationServer: @unchecked Sendable {
    private let listener: NWListener
    private let queue = DispatchQueue(label: "tv-navigation-fixture")

    private let itemCount: Int

    init(itemCount: Int = 416) throws {
        self.itemCount = itemCount
        let parameters = NWParameters.tcp
        parameters.requiredLocalEndpoint = .hostPort(host: .ipv4(.loopback), port: .any)
        listener = try NWListener(using: parameters)
    }

    func start() throws -> String {
        listener.newConnectionHandler = { [weak self] connection in
            guard let self else { connection.cancel(); return }
            connection.start(queue: self.queue)
            self.receive(connection, buffered: Data())
        }
        let ready = DispatchSemaphore(value: 0)
        listener.stateUpdateHandler = { [weak self] state in
            switch state {
            case .ready, .failed:
                self?.listener.stateUpdateHandler = nil
                ready.signal()
            default: break
            }
        }
        listener.start(queue: queue)
        guard ready.wait(timeout: .now() + 5) == .success, let port = listener.port else {
            listener.cancel()
            throw URLError(.cannotConnectToHost)
        }
        return "http://127.0.0.1:\(port.rawValue)"
    }

    func stop() { listener.cancel() }

    private func receive(_ connection: NWConnection, buffered: Data) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] chunk, _, complete, error in
            guard let self else { connection.cancel(); return }
            var data = buffered
            data.append(chunk ?? Data())
            guard let request = String(data: data, encoding: .utf8), request.contains("\r\n\r\n") else {
                if complete || error != nil { connection.cancel() }
                else { self.receive(connection, buffered: data) }
                return
            }
            let path = request.split(separator: " ").dropFirst().first.map(String.init) ?? "/"
            let body = self.response(path)
            var response = Data("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: \(body.count)\r\nConnection: close\r\n\r\n".utf8)
            response.append(body)
            connection.send(content: response, completion: .contentProcessed { _ in connection.cancel() })
        }
    }

    private func response(_ target: String) -> Data {
        let url = URLComponents(string: "http://fixture" + target)!
        let path = url.path
        let user: [String: Any] = ["id": 1, "username": "fixture"]
        let value: Any
        switch path {
        case "/api/v1/auth/login": value = ["token": "fixture-only", "user": user]
        case "/api/v1/me": value = user
        case "/api/v1/server": value = ["name": "Library fixture", "instance_id": "library-fixture"]
        case "/api/v1/libraries": value = [["id": 1, "name": "Movies", "kind": "movie"]]
        case "/api/v1/libraries/1/items":
            let query = Dictionary(uniqueKeysWithValues: (url.queryItems ?? []).map { ($0.name, $0.value ?? "") })
            let offset = Int(query["offset"] ?? "0") ?? 0
            let limit = Int(query["limit"] ?? "200") ?? 200
            var items: [[String: Any]] = (0..<itemCount).map { id in
                let letter = String(UnicodeScalar(65 + min(25, id * 26 / itemCount))!)
                let title = "\(letter) Movie \(id)"
                return ["id": id + 1, "library_id": 1, "kind": "movie", "title": title,
                        "sort_title": title.lowercased(), "year": 2000 + id / 16]
            }
            if query["sort"] == "year" { items.reverse() }
            value = ["items": Array(items.dropFirst(offset).prefix(limit)), "total": items.count]
        default: value = [:]
        }
        return try! JSONSerialization.data(withJSONObject: value)
    }
}
#endif
