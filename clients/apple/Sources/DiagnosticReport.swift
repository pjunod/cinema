import Foundation
import UIKit

enum DiagnosticReport {
    struct Component: Codable, Equatable {
        let name: String
        var state: String
        var reason: String?
        var valid: Bool {
            ["client","server","workers","connection","platform","clock"].contains(name)
                && ["complete","partial","unavailable"].contains(state)
                && (state == "complete" ? reason == nil : DiagnosticWire.reasons.contains(reason ?? ""))
        }
    }
    struct Binding: Codable, Equatable {
        let report_id: String
        let sha256: String
        let length: Int
        let privacy: Privacy
    }
    struct Privacy: Codable, Equatable { let titles_included: Bool; let attachments_included: Bool }
    struct Client: Codable { let platform: String; let app_version: String; let build: String; let os_version: String; let device_model: String }
    struct Capture: Codable { let mode: String; let window_ms: UInt64; let dropped_events: String; let persistence: String }
    struct File: Codable { let name: String; let size_bytes: Int; let sha256: String }
    struct Manifest: Codable {
        var version = DiagnosticWire.version
        let report_id: String
        let captured_at: String
        let category: String
        let scope: String
        let client: Client
        let capture: Capture
        let components: [Component]
        let privacy: Privacy
        var files: [File]
        var summary: String {
            let coverage = components.map { "\($0.name): \($0.state)" + ($0.reason.map { " (\($0))" } ?? "") }.joined(separator: "\n")
            return "Cinema diagnostic report \(report_id)\nCaptured: \(captured_at)\nCategory: \(category)\nClient: \(client.platform) \(client.app_version) build \(client.build)\nCapture: \(capture.mode) (\(capture.window_ms) ms); dropped \(capture.dropped_events); persistence \(capture.persistence)\n\(coverage)\nComplete describes collection scope, not retained history.\n"
        }
    }
    struct Archive { let bytes: Data; let binding: Binding; let manifest: Manifest }
    static var encoder: JSONEncoder { let value = JSONEncoder(); value.outputFormatting = [.sortedKeys]; return value }
    static func label(_ value: String?) -> String {
        let filtered = (value ?? "unknown").unicodeScalars.filter { $0.isASCII && (CharacterSet.alphanumerics.contains($0) || "._+-".unicodeScalars.contains($0)) }
        let text = String(String.UnicodeScalarView(filtered.prefix(64))); return text.isEmpty ? "unknown" : text
    }
    @MainActor static var client: Client {
        #if os(tvOS)
        let platform = "tvos"
        #else
        let platform = UIDevice.current.userInterfaceIdiom == .pad ? "ipados" : "ios"
        #endif
        return Client(platform: platform, app_version: label(Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String),
                      build: label(Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String),
                      os_version: label(UIDevice.current.systemVersion), device_model: label(UIDevice.current.model))
    }
    static func jsonl(_ events: [DiagnosticEvent]) throws -> Data {
        var bytes = Data(); var seen = Set<String>()
        let ordered = events.sorted { $0.run_id == $1.run_id ? (UInt64($0.sequence) ?? 0) < (UInt64($1.sequence) ?? 0) : $0.run_id < $1.run_id }
        for event in ordered {
            guard event.valid else { throw DiagnosticTransport.Failure.invalid }
            guard seen.insert(event.run_id + ":" + event.sequence).inserted else { continue }
            bytes.append(try encoder.encode(event)); bytes.append(10)
            guard bytes.count <= 8_388_608 else { throw DiagnosticTransport.Failure.limit }
        }
        return bytes
    }
    /// Call from a detached utility task. Captures contain no player objects.
    static func archive(_ draft: DiagnosticStore.Draft, client: Client) throws -> Archive {
        try Task.checkCancellation()
        guard ["playback","connection","interface","other"].contains(draft.category), draft.description.count <= 4_000,
              draft.description.utf8.count <= 16_384, draft.description.unicodeScalars.allSatisfy({ $0.value >= 32 || [9,10,13].contains($0.value) }),
              draft.capture.events.count <= 10_000, draft.serverEvents.count <= 10_000 else { throw DiagnosticTransport.Failure.invalid }
        let title = draft.title.map { String($0.unicodeScalars.filter { !CharacterSet.controlCharacters.contains($0) }.prefix(256)) }
        let platform = draft.includePlatform == true ? (draft.platform ?? []) : []
        let privacy = Privacy(titles_included: title != nil, attachments_included: !platform.isEmpty)
        let clientState = !draft.capture.historyEnabled && draft.capture.mode != "reproduction" ? Component(name: "client", state: "partial", reason: "history_disabled")
            : draft.capture.dropped > 0 ? Component(name: "client", state: "partial", reason: "overflow") : Component(name: "client", state: "complete")
        var components = [clientState]
        components += draft.serverComponents.filter { ["server","workers"].contains($0.name) && $0.valid }
        if !components.contains(where: { $0.name == "server" }) { components.append(Component(name: "server", state: "unavailable", reason: "context_unavailable")) }
        components += [Component(name: "connection", state: draft.connection.isEmpty ? "unavailable" : "complete", reason: draft.connection.isEmpty ? "not_requested" : nil),
                       Component(name: "platform", state: !platform.isEmpty ? "partial" : "unavailable", reason: (platform.isEmpty && draft.platform?.isEmpty == false ? "not_requested" : draft.platformReason) ?? (client.platform == "tvos" ? "unsupported" : "not_delivered")), Component(name: "clock", state: draft.connection.contains { $0.facts["clock_uncertainty_ms"] != nil } ? "complete" : "unavailable", reason: draft.connection.contains { $0.facts["clock_uncertainty_ms"] != nil } ? nil : "clock_unknown")]
        var manifest = Manifest(report_id: draft.id, captured_at: draft.capture.capturedAt, category: draft.category,
                                scope: draft.capture.namespace == "anonymous" ? "anonymous" : "viewer", client: client,
                                capture: Capture(mode: draft.capture.mode, window_ms: min(900_000, draft.capture.windowMs), dropped_events: String(draft.capture.dropped), persistence: draft.capture.persistence),
                                components: components, privacy: privacy, files: [])
        var entries: [(String, Data)] = [("summary.txt", Data(manifest.summary.utf8)), ("description.txt", Data(draft.description.utf8)), ("client-events.jsonl", try jsonl(draft.capture.events))]
        if title != nil {
            let eventData = try encoder.encode(draft.capture.events.filter { $0.domain == "playback" })
            let events = try JSONSerialization.jsonObject(with: eventData)
            entries.append(("playback.json", try JSONSerialization.data(withJSONObject: ["events": events, "title": title!], options: [.sortedKeys])))
        } else if draft.capture.events.contains(where: { $0.domain == "playback" }) {
            entries.append(("playback.json", try encoder.encode(draft.capture.events.filter { $0.domain == "playback" })))
        }
        if !platform.isEmpty { guard platform.allSatisfy(\.valid), platform.count <= 16 else { throw DiagnosticTransport.Failure.invalid }; entries.append(("diagnostics/platform.json", try encoder.encode(platform))) }
        if !draft.connection.isEmpty { entries.append(("connection.json", try encoder.encode(draft.connection))) }
        let system = draft.serverEvents.filter { $0.facts["active_producers"] != nil }
        if !system.isEmpty { entries.append(("server-snapshot.json", try encoder.encode(system))) }
        if !draft.serverEvents.isEmpty { entries.append(("server-events.jsonl", try jsonl(draft.serverEvents))) }
        manifest.files = entries.map { File(name: $0.0, size_bytes: $0.1.count, sha256: DiagnosticRecorder.digest($0.1)) }
        let bytes = try encoder.encode(manifest); _ = try DiagnosticWire.manifest(bytes)
        entries.append(("manifest.json", bytes)); let archive = try zip(entries)
        return Archive(bytes: archive, binding: Binding(report_id: draft.id, sha256: DiagnosticRecorder.digest(archive), length: archive.count, privacy: privacy), manifest: manifest)
    }
    static func zip(_ entries: [(String, Data)]) throws -> Data {
        guard entries.count <= 32 else { throw DiagnosticTransport.Failure.limit }
        var result = Data(); var central = Data()
        for (name, bytes) in entries {
            try Task.checkCancellation()
            guard DiagnosticWire.names.contains(name) || name == "manifest.json", bytes.count <= 8_388_608 else { throw DiagnosticTransport.Failure.limit }
            let path = Data(name.utf8); let offset = result.count; var crc: UInt32 = 0xffff_ffff
            for (index, byte) in bytes.enumerated() {
                if index % 65_536 == 0 { try Task.checkCancellation() }
                crc ^= UInt32(byte); for _ in 0..<8 { crc = crc & 1 == 1 ? 0xedb8_8320 ^ (crc >> 1) : crc >> 1 }
            }
            crc ^= 0xffff_ffff
            result.little(UInt32(0x04034b50)); result.little(UInt16(20)); result.little(UInt16(0x800)); result.little(UInt16(0)); result.little(UInt32(0))
            result.little(crc); result.little(UInt32(bytes.count)); result.little(UInt32(bytes.count)); result.little(UInt16(path.count)); result.little(UInt16(0)); result.append(path); result.append(bytes)
            central.little(UInt32(0x02014b50)); central.little(UInt16(20)); central.little(UInt16(20)); central.little(UInt16(0x800)); central.little(UInt16(0)); central.little(UInt32(0))
            central.little(crc); central.little(UInt32(bytes.count)); central.little(UInt32(bytes.count)); central.little(UInt16(path.count)); central.little(UInt16(0)); central.little(UInt16(0)); central.little(UInt16(0)); central.little(UInt16(0)); central.little(UInt32(0)); central.little(UInt32(offset)); central.append(path)
            guard result.count + central.count + 22 <= 16_777_216 else { throw DiagnosticTransport.Failure.limit }
        }
        let offset = result.count; result.append(central); result.little(UInt32(0x06054b50)); result.little(UInt16(0)); result.little(UInt16(0))
        result.little(UInt16(entries.count)); result.little(UInt16(entries.count)); result.little(UInt32(central.count)); result.little(UInt32(offset)); result.little(UInt16(0)); return result
    }
}

private extension Data {
    mutating func little<T: FixedWidthInteger>(_ number: T) { var n = number.littleEndian; Swift.withUnsafeBytes(of: &n) { append(contentsOf: $0) } }
}
