import Foundation

enum SplitHLSAudio {
    static let capability = "hls-split-av-v1"

    /// Only the server's own rendition group has stable source-track indices.
    /// Require the same master to name the group on a video variant.
    static func ownsMaster(_ url: URL, origin: String, session: String) -> Bool {
        guard !session.isEmpty,
              var components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              components.user == nil, components.password == nil, components.fragment == nil,
              components.percentEncodedPath == "/api/v1/hls/\(session)/master.m3u8" else { return false }
        components.path = ""
        components.query = nil
        guard let authority = components.string,
              let expected = Session.canonicalOrigin(origin) else { return false }
        return Session.canonicalOrigin(authority) == expected
    }

    struct Rendition {
        let index: Int
        let name: String
        let uri: String
        let isDefault: Bool
    }

    static func names(in body: String) -> [Int: String] {
        let rows = renditions(in: body)
        let counts = Dictionary(grouping: rows, by: \.name)
        return Dictionary(uniqueKeysWithValues: rows.compactMap {
            counts[$0.name]?.count == 1 ? ($0.index, $0.name) : nil
        })
    }

    static func renditions(in body: String) -> [Rendition] {
        var rows: [Int: Rendition] = [:]
        var group: String?
        var videoGroups = Set<String>()
        var pendingGroup: String?
        for raw in body.split(whereSeparator: \.isNewline) {
            let line = String(raw)
            if line.hasPrefix("#EXT-X-STREAM-INF:") {
                guard let variant = attributes(String(line.dropFirst("#EXT-X-STREAM-INF:".count))) else { return [] }
                pendingGroup = variant["AUDIO"]
            } else if !line.hasPrefix("#"), let candidate = pendingGroup {
                guard isChild(line, role: "video") else { return [] }
                videoGroups.insert(candidate)
                pendingGroup = nil
            } else if line.hasPrefix("#EXT-X-MEDIA:") {
                guard let media = attributes(String(line.dropFirst("#EXT-X-MEDIA:".count))) else { return [] }
                guard media["TYPE"] == "AUDIO" else { continue }
                guard let id = media["STABLE-RENDITION-ID"], id.first == "a",
                      let index = Int(id.dropFirst()), index >= 0, id == "a\(index)",
                      let uri = media["URI"], isChild(uri, role: "audio"),
                      let name = media["NAME"], !name.isEmpty,
                      let audioGroup = media["GROUP-ID"], !audioGroup.isEmpty,
                      group == nil || group == audioGroup, rows[index] == nil else { return [] }
                rows[index] = Rendition(index: index, name: name, uri: uri, isDefault: media["DEFAULT"] == "YES")
                group = audioGroup
            }
        }
        guard let group, videoGroups == [group], pendingGroup == nil,
              rows.values.filter(\.isDefault).count == 1 else { return [] }
        return Array(rows.values)
    }

    private static func isChild(_ value: String, role: String) -> Bool {
        let parts = value.split(separator: "/", omittingEmptySubsequences: false)
        return parts.count == 3 && parts[0] == role && parts[2] == "index.m3u8"
            && parts[1].utf8.count == 64
            && parts[1].utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }

    private static func attributes(_ list: String) -> [String: String]? {
        var remaining = list[...]
        var values: [String: String] = [:]
        while !remaining.isEmpty {
            guard let equals = remaining.firstIndex(of: "=") else { return nil }
            let key = String(remaining[..<equals])
            remaining = remaining[remaining.index(after: equals)...]
            let value: Substring
            if remaining.first == "\"" {
                remaining = remaining.dropFirst()
                guard let end = remaining.firstIndex(of: "\"") else { return nil }
                value = remaining[..<end]
                remaining = remaining[remaining.index(after: end)...]
            } else {
                let end = remaining.firstIndex(of: ",") ?? remaining.endIndex
                value = remaining[..<end]
                remaining = remaining[end...]
            }
            guard !key.isEmpty, values.updateValue(String(value), forKey: key) == nil else { return nil }
            if !remaining.isEmpty {
                guard remaining.first == "," else { return nil }
                remaining = remaining.dropFirst()
            }
        }
        return values
    }
}
