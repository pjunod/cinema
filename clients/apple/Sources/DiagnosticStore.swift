import Foundation

/// All disk access belongs to this actor. No file contains support authority.
actor DiagnosticStore {
    static let shared = DiagnosticStore()
    struct Ring: Codable { var events: [DiagnosticEvent]; var dropped: UInt64; var persistence: String; var interrupted: Bool }
    struct Draft: Codable, Identifiable {
        var id: String { capture.id }
        var capture: DiagnosticCapture
        var category: String
        var description: String
        var title: String?
        var serverEvents: [DiagnosticEvent]
        var serverComponents: [DiagnosticReport.Component]
        var connection: [DiagnosticEvent]
        var platform: [DiagnosticEvent]?
        var platformReason: String?
        var includePlatform: Bool?
        var consented = false
        var origin: String?
        var state = "local"
        var reference: String?
        var handoff: DiagnosticTransport.Offer?
        var grantExpires: UInt64?
        var binding: DiagnosticReport.Binding?
        var manifest: DiagnosticReport.Manifest?
    }
    private var memory: [String: Draft] = [:]
    private var archives: [String: Data] = [:]
    private var retired: Set<String> = []
    private var deleted: Set<String> = []
    private var ringMemory: [String: Ring] = [:]
    private let rootOverride: URL?
    private let authorizationGeneration: () -> UInt64
    init(root: URL? = nil, authorizationGeneration: @escaping () -> UInt64 = { Session.shared.playbackAuthorization.generation }) {
        rootOverride = root; self.authorizationGeneration = authorizationGeneration
    }
    private var root: URL? {
        if let rootOverride { return rootOverride }
        guard let url = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first else { return nil }
        return url.appendingPathComponent("CinemaDiagnostics", isDirectory: true)
    }
    private func directory(_ scope: String) throws -> URL {
        guard let root, scope == "anonymous" || DiagnosticWire.matches(scope, "^[a-f0-9]{64}$"), !retired.contains(scope) else { throw DiagnosticTransport.Failure.storage }
        let url = root.appendingPathComponent(scope, isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        var protected = url; var values = URLResourceValues(); values.isExcludedFromBackup = true; try protected.setResourceValues(values)
        return url
    }
    private func read<T: Decodable>(_ type: T.Type, _ url: URL, limit: Int) throws -> T {
        let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
        guard (attributes[.type] as? FileAttributeType) == .typeRegular, ((attributes[.size] as? NSNumber)?.intValue ?? Int.max) <= limit else { throw DiagnosticTransport.Failure.storage }
        let bytes = try Data(contentsOf: url, options: .mappedIfSafe)
        guard bytes.count <= limit else { throw DiagnosticTransport.Failure.storage }
        return try JSONDecoder().decode(type, from: bytes)
    }
    private func write(_ bytes: Data, to url: URL) throws {
        try bytes.write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
        var protected = url; var values = URLResourceValues(); values.isExcludedFromBackup = true; try protected.setResourceValues(values)
    }
    func recoverRing(_ scope: String) -> Ring {
        var value = ringMemory[scope] ?? Ring(events: [], dropped: 0, persistence: "memory_only", interrupted: true)
        if let directory = try? directory(scope), let read = try? read(Ring.self, directory.appendingPathComponent("ring.json"), limit: 2_500_000) {
            value = read; value.persistence = "available"
        }
        let oldest = DiagnosticRecorder.utc(Date().addingTimeInterval(-600))
        value.events = value.events.filter { $0.valid && $0.utc >= oldest && $0.utc <= DiagnosticRecorder.utc() }
        if value.events.count > 10_000 { value.events = Array(value.events.suffix(10_000)); value.dropped = UInt64.max }
        var capture = DiagnosticCapture(id: UUID().uuidString.lowercased(), namespace: scope, generation: 0, capturedAt: DiagnosticRecorder.utc(), incidentMs: 0, events: value.events, dropped: value.dropped, persistence: value.persistence, historyEnabled: true)
        DiagnosticRecorder.trim(&capture); value.events = capture.events; value.dropped = capture.dropped
        // A previous marker proves interruption, never a crash. The next run
        // supplies a new ordered lifecycle event through the recorder itself.
        return value
    }
    func saveRing(_ capture: DiagnosticCapture) -> String {
        guard !retired.contains(capture.namespace), capture.generation == authorizationGeneration() else { return "unavailable" }
        let ring = Ring(events: capture.events, dropped: capture.dropped, persistence: "available", interrupted: true)
        ringMemory = [capture.namespace: ring]
        do { let url = try directory(capture.namespace).appendingPathComponent("ring.json"); try write(JSONEncoder().encode(ring), to: url); return "available" }
        catch { return "memory_only" }
    }
    func saveCapture(_ capture: DiagnosticCapture) {
        let draft = drafts(capture.namespace).first(where: { $0.id == capture.id }) ?? Draft(capture: capture, category: "other", description: "", serverEvents: [], serverComponents: [], connection: [])
        var updated = draft; updated.capture = capture; _ = save(updated)
    }
    @discardableResult func save(_ draft: Draft) -> String {
        guard !retired.contains(draft.capture.namespace), !deleted.contains(draft.id), draft.capture.generation == authorizationGeneration() else { return "unavailable" }
        // A queued UI snapshot cannot replace a consented archive's privacy
        // choices or issuer after its immutable binding has been persisted.
        if let prior = storedDraft(draft.id, scope: draft.capture.namespace), prior.binding != nil {
            guard draft.consented, prior.origin == draft.origin, prior.title == draft.title,
                  prior.includePlatform == draft.includePlatform,
                  prior.binding == draft.binding,
                  (try? DiagnosticReport.encoder.encode(prior.manifest)) == (try? DiagnosticReport.encoder.encode(draft.manifest)) else { return "unavailable" }
        }
        let safe = draft.capture.events.allSatisfy(\.valid) && draft.serverEvents.allSatisfy(\.valid) && draft.connection.allSatisfy(\.valid)
        guard safe, UUID(uuidString: draft.id)?.uuidString.lowercased() == draft.id, let bytes = try? JSONEncoder().encode(draft), bytes.count <= 8_388_608 else { return "unavailable" }
        memory[draft.id] = draft
        while memory.count > 10 || memory.values.reduce(0, { $0 + ((try? JSONEncoder().encode($1).count) ?? 0) }) > 33_554_432 {
            guard let oldest = memory.values.filter({ $0.id != draft.id }).min(by: { $0.capture.incidentMs < $1.capture.incidentMs }) else { break }
            memory.removeValue(forKey: oldest.id)
        }
        do {
            let directory = try directory(draft.capture.namespace)
            try prune(directory)
            try write(bytes, to: directory.appendingPathComponent(draft.id + ".json"))
            try prune(directory); return "available"
        } catch { return "memory_only" }
    }
    private func pruneLocal(_ directory: URL) throws {
        let urls = try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: [.fileSizeKey, .contentModificationDateKey, .isRegularFileKey])
        guard urls.count <= 32 else { throw DiagnosticTransport.Failure.storage }
        var rows: [(URL, Int, Date)] = []
        for url in urls where url.lastPathComponent != "ring.json" {
            let values = try url.resourceValues(forKeys: [.fileSizeKey, .contentModificationDateKey, .isRegularFileKey])
            guard values.isRegularFile == true else { continue }
            let size = values.fileSize ?? Int.max; let date = values.contentModificationDate ?? .distantPast
            if size > 8_388_608 || date < Date().addingTimeInterval(-86_400) { try FileManager.default.removeItem(at: url); DiagnosticTransport.clearSecrets(url.deletingPathExtension().lastPathComponent, scope: directory.lastPathComponent) }
            else { rows.append((url, size, date)) }
        }
        rows.sort { $0.2 < $1.2 }; var total = rows.reduce(0) { $0 + $1.1 }
        while Set(rows.map { $0.0.deletingPathExtension().lastPathComponent }).count > 10 || total > 33_554_432 {
            let id = rows[0].0.deletingPathExtension().lastPathComponent
            for row in rows where row.0.deletingPathExtension().lastPathComponent == id { total -= row.1; try FileManager.default.removeItem(at: row.0) }
            rows.removeAll { $0.0.deletingPathExtension().lastPathComponent == id }; memory.removeValue(forKey: id); archives.removeValue(forKey: id)
            DiagnosticTransport.clearSecrets(id, scope: directory.lastPathComponent)
        }
    }
    private func prune(_ directory: URL) throws {
        guard let root else { return }
        let folders = try FileManager.default.contentsOfDirectory(at: root, includingPropertiesForKeys: [.isDirectoryKey])
        var rows: [(URL, Int, Date)] = []
        for folder in folders where folder.lastPathComponent == "anonymous" || DiagnosticWire.matches(folder.lastPathComponent, "^[a-f0-9]{64}$") {
            guard (try? folder.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) == true else { continue }
            try pruneLocal(folder)
            for url in try FileManager.default.contentsOfDirectory(at: folder, includingPropertiesForKeys: [.fileSizeKey, .contentModificationDateKey, .isRegularFileKey]) {
                let values = try url.resourceValues(forKeys: [.fileSizeKey, .contentModificationDateKey, .isRegularFileKey])
                guard values.isRegularFile == true else { continue }
                let date = values.contentModificationDate ?? .distantPast
                if url.lastPathComponent == "ring.json" {
                    if date < Date().addingTimeInterval(-600) { try FileManager.default.removeItem(at: url) }
                } else { rows.append((url, values.fileSize ?? Int.max, date)) }
            }
            if (try FileManager.default.contentsOfDirectory(atPath: folder.path)).isEmpty && folder != directory {
                try FileManager.default.removeItem(at: folder); DiagnosticTransport.clearSecrets("enrollment", scope: folder.lastPathComponent)
            }
        }
        rows.sort { $0.2 < $1.2 }; var total = rows.reduce(0) { $0 + $1.1 }
        while Set(rows.map { $0.0.deletingPathExtension().lastPathComponent }).count > 10 || total > 33_554_432 {
            let id = rows[0].0.deletingPathExtension().lastPathComponent
            for row in rows where row.0.deletingPathExtension().lastPathComponent == id {
                total -= row.1; try FileManager.default.removeItem(at: row.0)
                for key in [id, id + ":invitation", id + ":invitation-proof"] { DiagnosticTransport.clearSecrets(key, scope: row.0.deletingLastPathComponent().lastPathComponent) }
            }
            rows.removeAll { $0.0.deletingPathExtension().lastPathComponent == id }; memory.removeValue(forKey: id); archives.removeValue(forKey: id)
        }
    }
    func drafts(_ scope: String) -> [Draft] {
        if let directory = try? directory(scope) {
            try? prune(directory)
            let urls = (try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)) ?? []
            for url in urls.prefix(32) where url.lastPathComponent != "ring.json" && url.pathExtension == "json" {
                if let draft = try? read(Draft.self, url, limit: 8_388_608), !deleted.contains(draft.id), draft.capture.namespace == scope,
                   draft.id + ".json" == url.lastPathComponent, draft.capture.events.allSatisfy(\.valid), memory[draft.id] == nil { memory[draft.id] = draft }
            }
        }
        return Array(memory.values.filter { $0.capture.namespace == scope && Double($0.capture.incidentMs) >= Date().addingTimeInterval(-86_400).timeIntervalSince1970 * 1_000 }.sorted { $0.capture.incidentMs > $1.capture.incidentMs }.prefix(10))
    }
    func saveArchive(_ bytes: Data, id: String, scope: String, generation: UInt64) -> String {
        guard bytes.count <= 8_388_608, UUID(uuidString: id)?.uuidString.lowercased() == id, !retired.contains(scope), !deleted.contains(id), generation == authorizationGeneration() else { return "unavailable" }
        if let binding = storedDraft(id, scope: scope)?.binding {
            guard bytes.count == binding.length, DiagnosticRecorder.digest(bytes) == binding.sha256 else { return "unavailable" }
        }
        archives = [id: bytes]
        do { let folder = try directory(scope); try write(bytes, to: folder.appendingPathComponent(id + ".zip")); try prune(folder); return "available" }
        catch { return "memory_only" }
    }
    func archive(_ id: String, scope: String) -> Data? {
        guard UUID(uuidString: id)?.uuidString.lowercased() == id, !retired.contains(scope), !deleted.contains(id) else { return nil }
        if let value = archives[id] { return value }
        guard let folder = try? directory(scope) else { return nil }; let url = folder.appendingPathComponent(id + ".zip")
        guard let attrs = try? FileManager.default.attributesOfItem(atPath: url.path), (attrs[.type] as? FileAttributeType) == .typeRegular,
              ((attrs[.size] as? NSNumber)?.intValue ?? Int.max) <= 8_388_608 else { return nil }
        return try? Data(contentsOf: url)
    }
    func delete(_ id: String, scope: String) {
        guard UUID(uuidString: id)?.uuidString.lowercased() == id else { return }
        deleted.insert(id)
        memory.removeValue(forKey: id); archives.removeValue(forKey: id)
        for key in [id, id + ":invitation", id + ":invitation-proof"] { DiagnosticTransport.clearSecrets(key, scope: scope) }
        if let directory = try? directory(scope) { for ext in ["json","zip"] { try? FileManager.default.removeItem(at: directory.appendingPathComponent(id + "." + ext)) } }
    }
    private func storedDraft(_ id: String, scope: String) -> Draft? {
        if let value = memory[id], value.capture.namespace == scope { return value }
        guard let folder = try? directory(scope), let value = try? read(Draft.self, folder.appendingPathComponent(id + ".json"), limit: 8_388_608), value.id == id, value.capture.namespace == scope else { return nil }
        return value
    }
    func clearRing(_ scope: String) {
        ringMemory.removeValue(forKey: scope)
        if let directory = try? directory(scope) { try? FileManager.default.removeItem(at: directory.appendingPathComponent("ring.json")) }
    }
    func clear(_ scope: String) {
        for draft in drafts(scope) { for key in [draft.id, draft.id + ":invitation", draft.id + ":invitation-proof"] { DiagnosticTransport.clearSecrets(key, scope: scope) } }
        DiagnosticTransport.clearSecrets("enrollment", scope: scope)
        memory = memory.filter { $0.value.capture.namespace != scope }; archives = [:]; ringMemory.removeValue(forKey: scope)
        if let directory = try? directory(scope) { try? FileManager.default.removeItem(at: directory) }
        retired.insert(scope)
    }
    func activate(_ scope: String) { retired.remove(scope) }
}
