import Foundation
import CryptoKit

/// Closed values, rather than arbitrary log messages, are the recorder boundary.
enum DiagnosticFact: Codable, Equatable {
    case number(UInt64), text(String), flag(Bool)
    init(from decoder: Decoder) throws {
        let value = try decoder.singleValueContainer()
        if let bool = try? value.decode(Bool.self) { self = .flag(bool) }
        else if let number = try? value.decode(UInt64.self) { self = .number(number) }
        else { self = .text(try value.decode(String.self)) }
    }
    func encode(to encoder: Encoder) throws {
        var value = encoder.singleValueContainer()
        switch self { case .number(let n): try value.encode(n); case .text(let s): try value.encode(s); case .flag(let b): try value.encode(b) }
    }
    func allowed(_ key: String) -> Bool {
        switch self {
        case .number(let n):
            if key == "status" { return n == 0 || (100...599).contains(n) }
            return ["duration_ms", "playhead_ms", "headroom_ms", "width", "height", "retry_count", "clock_ahead_ms", "clock_behind_ms", "clock_uncertainty_ms", "active_producers", "hw_slots", "hw_max"].contains(key) && n <= 604_800_000
        case .flag: return key == "observed"
        case .text(let s):
            if ["frame_count", "dropped_frames", "dropped_events"].contains(key) { return DiagnosticWire.decimal(s) }
            if key == "correlation" { return DiagnosticWire.matches(s, "^s-[a-f0-9]{64}$") }
            if ["app_version", "build"].contains(key) { return DiagnosticWire.matches(s, "^[A-Za-z0-9._+-]{1,64}$") }
            if key == "node" { return DiagnosticWire.matches(s, "^node-[1-8]$") }

            return Self.enums[key]?.contains(s) == true
        }
    }
    private static let enums: [String: [String]] = [
                "source_codec": ["unknown","h264","hevc","av1","vp9","aac","ac3","eac3","dts","truehd","opus","flac","pcm","other"],
                "delivered_codec": ["unknown","h264","hevc","av1","vp9","aac","ac3","eac3","dts","truehd","opus","flac","pcm","other"],
                "render_codec": ["unknown","h264","hevc","av1","vp9","aac","ac3","eac3","dts","truehd","opus","flac","pcm","other"],
                "audio_codec": ["unknown","h264","hevc","av1","vp9","aac","ac3","eac3","dts","truehd","opus","flac","pcm","other"],
                "source_range": ["unknown","sdr","hdr10","hlg","dv"], "delivered_range": ["unknown","sdr","hdr10","hlg","dv"], "render_range": ["unknown","sdr","hdr10","hlg","dv"],
                "mode": ["unknown","direct","remux","transcode"],
                "failure": DiagnosticWire.reasons + ["network","tls","dns","connect","authentication","http","decode","delivery","process_exit","browser_network"],
                "reason": DiagnosticWire.reasons + ["network","tls","dns","connect","authentication","http","decode","delivery","process_exit","browser_network"],
                "action": ["play","pause","stop","seek","audio","subtitle","quality","navigate","report","reproduce","cancel","check"],
                "state": ["unknown","starting","playing","paused","waiting","stalled","recovered","failed","stopped","foreground","background","interrupted","clean_shutdown"],
                "route": ["auth","browse","playback","settings","diagnostics","support","other"], "method": ["GET","POST","PUT","DELETE","HEAD"],
                "phase": ["dns","connect","tls","auth","readiness","delivery","unavailable"]
            ]
}

struct DiagnosticEvent: Codable, Equatable {
    let run_id: String
    let sequence: String
    let utc: String
    let elapsed_ms: UInt64
    let domain: String
    let kind: String
    let facts: [String: DiagnosticFact]
    var valid: Bool {
        UUID(uuidString: run_id)?.uuidString.lowercased() == run_id && DiagnosticWire.decimal(sequence)
            && DiagnosticRecorder.validUTC(utc) && elapsed_ms <= 604_800_000
            && ["ui","network","playback","producer","lifecycle","coverage"].contains(domain)
            && ["action","state","sample","error","terminal","request","availability"].contains(kind)
            && facts.count <= 16 && facts.allSatisfy { $0.value.allowed($0.key) }
    }
}

struct DiagnosticCapture: Codable, Identifiable {
    let id: String
    let namespace: String
    let generation: UInt64
    let capturedAt: String
    let incidentMs: UInt64
    var events: [DiagnosticEvent]
    var dropped: UInt64
    var persistence: String
    var historyEnabled: Bool
    var mode = "ordinary"
    var windowMs: UInt64 = 600_000
}

/// Callbacks do no I/O and never wait for serialization or an export owner.
/// The only timers belong to persistence and the explicitly requested recording.
final class DiagnosticRecorder: @unchecked Sendable {
    static let shared = DiagnosticRecorder()
    static let maximumBytes = 2 * 1024 * 1024
    static func digest(_ data: Data) -> String { SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined() }
    static func utc(_ date: Date = Date()) -> String {
        let format = DateFormatter(); format.locale = Locale(identifier: "en_US_POSIX")
        format.calendar = Calendar(identifier: .gregorian); format.timeZone = TimeZone(secondsFromGMT: 0)
        format.dateFormat = "yyyy-MM-dd'T'HH:mm:ss'Z'"; return format.string(from: date)
    }
    static func validUTC(_ value: String) -> Bool {
        let format = DateFormatter(); format.locale = Locale(identifier: "en_US_POSIX")
        format.calendar = Calendar(identifier: .gregorian); format.timeZone = TimeZone(secondsFromGMT: 0)
        format.dateFormat = "yyyy-MM-dd'T'HH:mm:ss'Z'"; format.isLenient = false
        guard value.count == 20, let date = format.date(from: value) else { return false }
        return format.string(from: date) == value
    }
    private struct Row { let event: DiagnosticEvent; let size: Int }
    private let lock = NSLock()
    private let lossLock = NSLock()
    private var contention: UInt64 = 0
    private let writer = DispatchQueue(label: "cinema.diagnostics.writer", qos: .utility)
    private let persistenceEnabled: Bool
    init(persistenceEnabled: Bool = true) { self.persistenceEnabled = persistenceEnabled }
    private var flushing = false
    private let run = UUID().uuidString.lowercased()
    private let timestampFormat: DateFormatter = {
        let format = DateFormatter(); format.locale = Locale(identifier: "en_US_POSIX")
        format.calendar = Calendar(identifier: .gregorian); format.timeZone = TimeZone(secondsFromGMT: 0)
        format.dateFormat = "yyyy-MM-dd'T'HH:mm:ss'Z'"; return format
    }()
    private let started = ProcessInfo.processInfo.systemUptime
    private var sequence: UInt64 = 0
    private var dropped: UInt64 = 0
    private var rows: [Row] = []
    private var bytes = 0
    private var namespace = "anonymous"
    private var generation: UInt64 = Session.shared.playbackAuthorization.generation
    private var bound = false
    private var persistence = "memory_only"
    private var completedReproduction: DiagnosticCapture?
    private var reproduction: (capture: DiagnosticCapture, sizes: [Int], bytes: Int, deadline: TimeInterval)?
    private var reproductionTimer: DispatchWorkItem?
    var historyEnabled: Bool { UserDefaults.standard.object(forKey: "plurx.diagnosticHistory") as? Bool ?? true }

    func retireAuthorization(to auth: UInt64) {
        lock.lock(); generation = auth; namespace = "anonymous"; bound = false
        rows = []; bytes = 0; dropped = 0; completedReproduction = nil; reproduction = nil; reproductionTimer?.cancel(); reproductionTimer = nil
        lossLock.lock(); contention = 0; lossLock.unlock(); lock.unlock()
    }

    func bind(namespace value: String, generation auth: UInt64, erasePrevious: Bool = false) async {
        guard Session.shared.playbackAuthorization.generation == auth else { return }
        let binding = lock.withLock { () -> (String, Bool, Bool)? in
            guard Session.shared.playbackAuthorization.generation == auth else { return nil }
            let previous = namespace; let changed = value != previous
            if bound && changed { rows.removeAll(); bytes = 0; dropped = 0; completedReproduction = nil; reproduction = nil; reproductionTimer?.cancel() }
            namespace = value; generation = auth; let recover = !bound || changed; bound = true
            return (previous, changed, recover)
        }
        guard let (previous, changed, recover) = binding else { return }
        if erasePrevious && changed { await DiagnosticStore.shared.clear(previous) }
        if recover {
            let restored = await DiagnosticStore.shared.recoverRing(value)
            lock.withLock {
                if namespace == value && generation == auth && Session.shared.playbackAuthorization.generation == auth {
                    let current = rows; rows = []; bytes = 0
                    for event in restored.events + current.map(\.event) { append(event) }
                    dropped = dropped.addingSaturated(restored.dropped)
                    persistence = restored.persistence
                }
            }
            if restored.interrupted && !restored.events.isEmpty { record("lifecycle", "state", ["state": .text("interrupted"), "reason": .text("interrupted")]) }
            record("lifecycle", "state", ["state": .text("foreground")])
        }
    }
    func setHistory(_ enabled: Bool) async {
        UserDefaults.standard.set(enabled, forKey: "plurx.diagnosticHistory")
        let scope = lock.withLock { rows = []; bytes = 0; return namespace }
        await DiagnosticStore.shared.clearRing(scope)
    }
    func record(_ domain: String, _ kind: String, _ facts: [String: DiagnosticFact] = [:], generation expected: UInt64? = nil) {
        guard facts.count <= 16,
              ["ui","network","playback","producer","lifecycle","coverage"].contains(domain),
              ["action","state","sample","error","terminal","request","availability"].contains(kind) else { return }
        guard lock.try() else {
            lossLock.lock(); contention = contention.addingSaturated(1); lossLock.unlock(); return
        }
        defer { lock.unlock() }
        guard (historyEnabled || reproduction != nil), expected == nil || expected == generation else { return }
        sequence = sequence.addingSaturated(1)
        let event = DiagnosticEvent(run_id: run, sequence: String(sequence), utc: timestampFormat.string(from: Date()),
                                    elapsed_ms: min(604_800_000, UInt64(max(0, (ProcessInfo.processInfo.systemUptime - started) * 1_000))),
                                    domain: domain, kind: kind, facts: facts.filter { $0.value.allowed($0.key) })
        append(event)
        if reproduction != nil, let encoded = try? JSONEncoder().encode(event) {
            let size = encoded.count + 1
            if event.kind == "sample" && (reproduction!.capture.events.count >= 8_000 || reproduction!.bytes + size > Self.maximumBytes * 4 / 5) {
                reproduction!.capture.dropped = reproduction!.capture.dropped.addingSaturated(1)
            } else {
                while reproduction!.capture.events.count >= 10_000 || reproduction!.bytes + size > Self.maximumBytes {
                    let index = reproduction!.capture.events.firstIndex { $0.kind == "sample" } ?? 0
                    reproduction!.capture.events.remove(at: index); reproduction!.bytes -= reproduction!.sizes.remove(at: index)
                    reproduction!.capture.dropped = reproduction!.capture.dropped.addingSaturated(1)
                }
                reproduction!.capture.events.append(event); reproduction!.sizes.append(size); reproduction!.bytes += size
            }
        }
        guard persistenceEnabled, !flushing else { return }; flushing = true
        writer.asyncAfter(deadline: .now() + 3) { [weak self] in self?.flush() }
    }
    private func append(_ event: DiagnosticEvent) {
        guard let encoded = try? JSONEncoder().encode(event) else { return }
        let size = encoded.count + 1
        let threshold = Date().addingTimeInterval(-600)
        let oldest = timestampFormat.string(from: threshold)
        while let first = rows.first, first.event.utc < oldest { bytes -= first.size; rows.removeFirst() }
        if event.kind == "sample" && (rows.count >= 8_000 || bytes + size > Self.maximumBytes * 4 / 5) { dropped = dropped.addingSaturated(1); return }
        while rows.count >= 10_000 || bytes + size > Self.maximumBytes {
            let index = rows.firstIndex { $0.event.kind == "sample" } ?? 0
            bytes -= rows.remove(at: index).size; dropped = dropped.addingSaturated(1)
        }
        rows.append(Row(event: event, size: size)); bytes += size
    }
    static func trim(_ capture: inout DiagnosticCapture) {
        var sizes = capture.events.map { (try? JSONEncoder().encode($0).count).map { $0 + 1 } ?? 0 }
        var total = sizes.reduce(0, +)
        while capture.events.count > 10_000 || total > maximumBytes {
            let index = capture.events.firstIndex { $0.kind == "sample" } ?? 0
            capture.events.remove(at: index); total -= sizes.remove(at: index); capture.dropped = capture.dropped.addingSaturated(1)
        }
    }
    func freeze() -> DiagnosticCapture {
        lock.lock(); defer { lock.unlock() }
        let current = Session.shared.playbackAuthorization.generation
        if current != generation { generation = current; namespace = "anonymous"; rows = []; bytes = 0; dropped = 0; completedReproduction = nil; reproduction = nil }
        lossLock.lock(); let lost = contention; lossLock.unlock()
        return DiagnosticCapture(id: UUID().uuidString.lowercased(), namespace: namespace, generation: generation,
                                 capturedAt: Self.utc(), incidentMs: UInt64(max(0, Date().timeIntervalSince1970 * 1_000)),
                                 events: rows.map(\.event), dropped: dropped.addingSaturated(lost), persistence: persistence, historyEnabled: historyEnabled)
    }
    func startReproduction(_ capture: DiagnosticCapture) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard reproduction == nil, capture.namespace == namespace, capture.generation == generation else { return false }
        var value = capture; value.mode = "reproduction"; completedReproduction = nil
        let sizes = value.events.map { ((try? JSONEncoder().encode($0).count) ?? 0) + 1 }
        reproduction = (value, sizes, sizes.reduce(0, +), ProcessInfo.processInfo.systemUptime + 300)
        let task = DispatchWorkItem { [weak self] in _ = self?.stopReproduction(reason: "timeout") }
        reproductionTimer = task; writer.asyncAfter(deadline: .now() + 300, execute: task); return true
    }
    @discardableResult func stopReproduction(reason: String = "cancelled") -> DiagnosticCapture? {
        record("lifecycle", "terminal", ["state": .text(reason == "interrupted" ? "interrupted" : "stopped"), "reason": .text(reason)])
        lock.lock(); guard var value = reproduction?.capture else { lock.unlock(); return nil }
        reproduction = nil; reproductionTimer?.cancel(); reproductionTimer = nil
        value.windowMs = min(900_000, value.windowMs + UInt64(max(0, Date().timeIntervalSince1970 * 1_000 - Double(value.incidentMs))))
        completedReproduction = value; lock.unlock()
        Task { await DiagnosticStore.shared.saveCapture(value) }; return value
    }
    func forgetReproduction(_ id: String? = nil) { lock.withLock { if id == nil || completedReproduction?.id == id { completedReproduction = nil } } }
    var retainedReproduction: DiagnosticCapture? { lock.withLock { reproduction?.capture ?? completedReproduction } }
    var recording: Bool { lock.lock(); defer { lock.unlock() }; return reproduction != nil }
    func background() { _ = stopReproduction(reason: "interrupted"); record("lifecycle", "state", ["state": .text("background")]); flush() }
    private func flush() {
        let capture = freeze(); lock.lock(); flushing = false; lock.unlock()
        Task {
            let result = await DiagnosticStore.shared.saveRing(capture)
            lock.withLock { if namespace == capture.namespace && generation == capture.generation { persistence = result } }
        }
    }
}

extension UInt64 {
    fileprivate func addingSaturated(_ other: UInt64) -> UInt64 { let (n, overflow) = addingReportingOverflow(other); return overflow ? .max : n }
}

#if os(iOS)
import MetricKit

/// MetricKit delivery windows may span several accounts. Only a window wholly
/// inside one continuously authenticated lifetime may contribute measurements.
/// Raw payload JSON, stacks, paths and addresses never cross this adapter.
final class DiagnosticPlatform: NSObject, MXMetricManagerSubscriber, @unchecked Sendable {
    static let shared = DiagnosticPlatform()
    private let lock = NSLock()
    private var started = false
    private var scope = "anonymous"
    private var generation: UInt64 = 0
    private var began = Date()
    private var events: [DiagnosticEvent] = []
    private var delivered = false
    func start() {
        guard lock.withLock({ if started { return false }; started = true; return true }) else { return }
        MXMetricManager.shared.add(self)
        didReceive(MXMetricManager.shared.pastDiagnosticPayloads)
    }
    func bind(_ namespace: String, generation: UInt64) {
        lock.withLock {
            if scope != namespace || self.generation != generation { events = []; began = Date() }
            scope = namespace; self.generation = generation
        }
    }
    func retire() { lock.withLock { scope = "anonymous"; events = []; began = Date(); generation = Session.shared.playbackAuthorization.generation } }
    func didReceive(_ payloads: [MXDiagnosticPayload]) {
        lock.withLock {
            if !payloads.isEmpty { delivered = true }
            guard scope != "anonymous", generation == Session.shared.playbackAuthorization.generation else { return }
            let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
            for payload in payloads.prefix(16) where payload.timeStampBegin >= began && payload.timeStampEnd <= Date() && payload.timeStampEnd >= payload.timeStampBegin {
                let run = UUID().uuidString.lowercased()
                func append(_ kind: String, _ facts: [String: DiagnosticFact]) {
                    guard events.count < 16 else { return }
                    events.append(DiagnosticEvent(run_id: run, sequence: String(events.count + 1), utc: DiagnosticRecorder.utc(), elapsed_ms: 0,
                                                  domain: "lifecycle", kind: kind, facts: facts))
                }
                for crash in (payload.crashDiagnostics ?? []).prefix(8) where crash.applicationVersion == version {
                    // UTC is delivery observation time, not an invented crash time.
                    append("terminal", ["failure": .text("process_exit"), "observed": .flag(true), "app_version": .text(DiagnosticReport.label(crash.applicationVersion)), "build": .text(DiagnosticReport.label(crash.metaData.applicationBuildVersion))])
                }
                for hang in (payload.hangDiagnostics ?? []).prefix(8) where hang.applicationVersion == version {
                    let ms = hang.hangDuration.converted(to: .milliseconds).value
                    if ms.isFinite && ms >= 0 { append("sample", ["duration_ms": .number(UInt64(min(ms, 604_800_000))), "state": .text("stalled"), "observed": .flag(true), "app_version": .text(DiagnosticReport.label(hang.applicationVersion)), "build": .text(DiagnosticReport.label(hang.metaData.applicationBuildVersion))]) }
                }
            }
        }
    }
    func projection(_ capture: DiagnosticCapture) -> (events: [DiagnosticEvent], reason: String) {
        lock.withLock {
            guard scope == capture.namespace, generation == capture.generation else { return ([], "context_unavailable") }
            return (events, !events.isEmpty || delivered ? "context_unavailable" : "not_delivered")
        }
    }
}
#else
final class DiagnosticPlatform {
    static let shared = DiagnosticPlatform()
    func start() {}
    func bind(_ namespace: String, generation: UInt64) {}
    func retire() {}
    func projection(_ capture: DiagnosticCapture) -> (events: [DiagnosticEvent], reason: String) { ([], "unsupported") }
}
#endif
