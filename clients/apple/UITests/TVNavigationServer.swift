#if os(tvOS)
import Network
import Foundation

final class TVNavigationServer: @unchecked Sendable {
    private let listener: NWListener
    private let queue = DispatchQueue(label: "tv-navigation-fixture")

    private let itemCount: Int
    private let includeShows: Bool
    private var failSearch: Bool

    func allowSearchSuccess() { queue.sync { failSearch = false } }
    private var requestCounts: [String: Int] = [:]
    private let epoch = Int64(Date().timeIntervalSince1970 * 1000)

    func requestCount(_ path: String) -> Int { queue.sync { requestCounts[path, default: 0] } }

    init(itemCount: Int = 416, includeShows: Bool = false, failSearch: Bool = false) throws {
        self.itemCount = itemCount
        self.includeShows = includeShows
        self.failSearch = failSearch
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
            let headers = request.components(separatedBy: "\r\n\r\n")[0]
            let lengthLine = headers.components(separatedBy: "\r\n").first { $0.lowercased().hasPrefix("content-length:") }
            let length = Int(lengthLine?.split(separator: ":").last?.trimmingCharacters(in: .whitespaces) ?? "0") ?? 0
            let headerEnd = data.range(of: Data("\r\n\r\n".utf8))!.upperBound
            if data.count < headerEnd + length {
                self.receive(connection, buffered: data)
                return
            }
            let requestBody = (try? JSONSerialization.jsonObject(with: data.suffix(length))) as? [String: Any] ?? [:]
            let plainPath = URLComponents(string: "http://fixture" + path)?.path ?? path
            self.requestCounts[plainPath, default: 0] += 1
            if plainPath == "/fixture.mp4" {
                self.serveVideo(connection, headers: headers)
                return
            }
            let body = self.response(path, requestBody: requestBody)
            let status = plainPath == "/api/v1/search" && self.failSearch ? "500 Internal Server Error" : "200 OK"
            var response = Data("HTTP/1.1 \(status)\r\nContent-Type: application/json\r\nContent-Length: \(body.count)\r\nConnection: close\r\n\r\n".utf8)
            response.append(body)
            connection.send(content: response, completion: .contentProcessed { _ in connection.cancel() })
        }
    }

    private func serveVideo(_ connection: NWConnection, headers: String) {
        let url = Bundle(for: TVNavigationTests.self).url(forResource: "navigation", withExtension: "mp4")!
        let video = try! Data(contentsOf: url)
        let rangeLine = headers.components(separatedBy: "\r\n").first { $0.lowercased().hasPrefix("range:") }
        var start = 0
        var end = video.count - 1
        if let rangeLine, let range = rangeLine.components(separatedBy: "bytes=").last {
            let bounds = range.split(separator: "-", omittingEmptySubsequences: false)
            start = Int(bounds[0]) ?? 0
            if bounds.count > 1, let requestedEnd = Int(bounds[1]) { end = min(end, requestedEnd) }
        }
        guard start >= 0, start <= end else { connection.cancel(); return }
        let body = video.subdata(in: start..<(end + 1))
        let status = rangeLine == nil ? "200 OK" : "206 Partial Content"
        let contentRange = rangeLine == nil ? "" : "Content-Range: bytes \(start)-\(end)/\(video.count)\r\n"
        var response = Data("HTTP/1.1 \(status)\r\nContent-Type: video/mp4\r\nAccept-Ranges: bytes\r\n\(contentRange)Content-Length: \(body.count)\r\nConnection: close\r\n\r\n".utf8)
        response.append(body)
        connection.send(content: response, completion: .contentProcessed { _ in connection.cancel() })
    }

    private func programme(title: String, ordinal: Int, start: Int64, end: Int64) -> [String: Any] {
        ["source": "local", "channel_id": "fixture-channel", "generation_id": "g", "cycle": 0,
         "ordinal": ordinal, "starts_at_ms": start, "ends_at_ms": end, "item_id": 1, "file_id": 1, "title": title]
    }

    private func response(_ target: String, requestBody: [String: Any]) -> Data {
        let url = URLComponents(string: "http://fixture" + target)!
        let path = url.path
        let user: [String: Any] = ["id": 1, "username": "fixture"]
        let value: Any
        switch path {
        case "/api/v1/auth/login": value = ["token": "fixture-only", "user": user]
        case "/api/v1/me": value = user
        case "/api/v1/server": value = ["name": "Library fixture", "instance_id": "library-fixture"]
        case "/api/v1/libraries":
            var libraries: [[String: Any]] = [["id": 1, "name": "Movies", "kind": "movie"]]
            if includeShows { libraries.append(["id": 2, "name": "Shows", "kind": "show"]) }
            value = libraries
        case "/api/v1/libraries/2/items":
            value = ["items": [["id": 10001, "library_id": 2, "kind": "show", "title": "Fixture show"]], "total": 1]
        case "/api/v1/items/1":
            let paragraphs = (0..<4).map { index in
                "Synopsis section \(index + 1). " + String(repeating: "A synthetic sentence for television reading. ", count: 5)
                + (index == 3 ? "FINAL SYNOPSIS LINE." : "")
            }
            value = ["item": ["id": 1, "library_id": 1, "kind": "movie", "title": "A Movie 0",
                              "overview": paragraphs.joined(separator: "\n\n")], "files": []]
        case "/api/v1/items/10001":
            value = ["item": ["id": 10001, "library_id": 2, "kind": "show", "title": "Fixture show"],
                     "files": [], "children": [["id": 10002, "library_id": 2, "kind": "season",
                                                   "title": "Fixture season", "season_number": 1]]]
        case "/api/v1/items/10002":
            value = ["item": ["id": 10002, "library_id": 2, "kind": "season", "title": "Fixture season"],
                     "files": [], "children": [["id": 10003, "library_id": 2, "kind": "episode",
                                                   "title": "Fixture episode", "season_number": 1, "episode_number": 1]]]
        case "/api/v1/items/10003":
            value = ["item": ["id": 10003, "library_id": 2, "kind": "episode", "title": "Fixture episode",
                              "overview": "Synthetic episode information without starting playback."], "files": []]
        case "/api/v1/search":
            value = ["results": [["id": 1, "library_id": 1, "kind": "movie", "title": "A Movie 0"]]]
        case "/api/v1/library-channels":
            let recipe: [String: Any] = ["version": 1, "library_ids": [1], "kinds": ["movie"],
                "genres_any": [], "tags_any": [], "keywords_any": [], "include_item_ids": [1],
                "include_show_ids": [], "exclude_item_ids": [], "exclude_show_ids": [],
                "ordering": "release_order", "include_specials": false, "auto_refresh": true, "match_all_in_scope": true]
            value = [["id": "fixture-channel", "owner_user_id": 1, "name": "Fixture channel",
                      "description": "Synthetic navigation video", "visibility": "personal", "enabled": true,
                      "revision": 1, "recipe": recipe, "seed": [0], "active_generation_id": "g",
                      "active_epoch_ms": epoch, "favourite": false, "created_at_ms": epoch,
                      "updated_at_ms": epoch, "source": "local"]]
        case "/api/v1/library-channels/guide":
            value = [programme(title: "On now fixture", ordinal: 0, start: epoch - 1000, end: epoch + 119000),
                     programme(title: "Future fixture", ordinal: 1, start: epoch + 119000, end: epoch + 239000)]
        case "/api/v1/library-channels/fixture-channel/resolve":
            value = ["source": "local", "channel_id": "fixture-channel", "definition_revision": 1,
                     "generation_id": "g", "occurrence": ["cycle": 0, "ordinal": 0], "server_now_ms": epoch,
                     "starts_at_ms": epoch - 1000, "ends_at_ms": epoch + 119000, "item_id": 1, "file_id": 1,
                     "position_ms": 1000, "capabilities": ["join_schedule": true, "watch_from_start": true,
                                                          "seek_while_following": false, "record": false]]
        case "/api/v1/files/1/decision":
            value = ["file_id": 1, "method": "direct_play", "play_url": "/fixture.mp4"]
        case "/api/v1/library-channels/fixture-channel/sessions":
            value = ["playback": ["session_id": "fixture", "playlist_url": "/fixture.mp4", "duration_ms": 120000,
                                  "media_origin_ms": 1000],
                     "library_channel": ["channel_id": "fixture-channel", "generation_id": "g", "cycle": 0,
                                         "ordinal": 0, "tune_sequence": requestBody["tune_sequence"] ?? 1,
                                         "starts_at_ms": epoch - 1000, "ends_at_ms": epoch + 119000]]
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
