import Foundation
import CoreFoundation

/// The normalized wire contract is shared with Rust/JS/Kotlin golden fixtures.
enum DiagnosticWire {
    static let version = "cinema.diagnostics.v1"
    static let reasons = ["connection_failed","timeout","unsupported","context_unavailable","telemetry_disabled","storage_denied","storage_full","overflow","peer_unavailable","cancelled","not_requested","interrupted","clock_unknown","revoked","history_disabled","not_delivered","node_limit"]
    static let names = ["summary.txt","description.txt","client-events.jsonl","playback.json","connection.json","server-events.jsonl","server-snapshot.json","diagnostics/platform.json"]
    enum Invalid: Error { case schema }
    static func check(_ ok: Bool) throws { if !ok { throw Invalid.schema } }
    static func object(_ value: Any?, _ required: [String], _ optional: [String] = []) throws -> [String: Any] {
        guard let o = value as? [String: Any] else { throw Invalid.schema }
        try check(Set(required).isSubset(of: Set(o.keys)) && Set(o.keys).isSubset(of: Set(required+optional)))
        return o
    }
    static func text(_ value: Any?) throws -> String { guard let s = value as? String else { throw Invalid.schema }; return s }
    static func one(_ value: Any?, _ values: [String]) throws { try check(values.contains(try text(value))) }
    static func integer(_ value: Any?, _ max: UInt64) throws -> UInt64 {
        guard let n = value as? NSNumber, CFGetTypeID(n) != CFBooleanGetTypeID(), n.doubleValue >= 0, n.doubleValue <= Double(max) else { throw Invalid.schema }
        return n.uint64Value
    }
    static func matches(_ s: String, _ pattern: String) -> Bool { s.range(of: pattern, options: .regularExpression) != nil }
    static func decimal(_ s: String) -> Bool { matches(s,"^(0|[1-9][0-9]{0,19})$") && UInt64(s) != nil }
    static func bounded(_ v: Any, _ depth: Int, _ count: inout Int) throws {
        count -= 1; try check(count >= 0 && depth <= 16 && !(v is NSNull))
        if let s = v as? String { try check(s.utf8.count <= 16384) }
        else if let o = v as? [String: Any] { for (k,x) in o { try check(k.utf8.count <= 128); try bounded(x,depth+1,&count) } }
        else if let a = v as? [Any] { for x in a { try bounded(x,depth+1,&count) } }
    }
    static func event(_ data: Data) throws -> DiagnosticEvent {
        try check(data.count <= 2048); try RemoteStrictJSON.check(data)
        _ = try object(JSONSerialization.jsonObject(with: data), ["run_id","sequence","utc","elapsed_ms","domain","kind","facts"])
        let value = try JSONDecoder().decode(DiagnosticEvent.self, from: data); try check(value.valid); return value
    }
    static func manifest(_ data: Data) throws -> [String: Any] {
        try check(data.count <= 65536);try RemoteStrictJSON.check(data)
        let raw = try JSONSerialization.jsonObject(with:data); var budget=60000;try bounded(raw,0,&budget)
        let m = try object(raw,["version","report_id","captured_at","category","scope","client","capture","components","privacy","files"])
        try check(try text(m["version"]) == version)
        let id = try text(m["report_id"]);try check(UUID(uuidString:id)?.uuidString.lowercased() == id)
        let time = try text(m["captured_at"])
        let df=DateFormatter();df.locale=Locale(identifier:"en_US_POSIX");df.calendar=Calendar(identifier:.gregorian);df.timeZone=TimeZone(secondsFromGMT:0);df.dateFormat="yyyy-MM-dd'T'HH:mm:ss'Z'";df.isLenient=false
        guard let date=df.date(from:time) else {throw Invalid.schema};try check(time.count==20 && df.string(from:date)==time)
        try one(m["category"],["playback","connection","interface","other"]);try one(m["scope"],["anonymous","viewer","administrator"])
        let client=try object(m["client"],["platform","app_version","build","os_version","device_model"])
        try one(client["platform"],["web","browser_tv","ios","ipados","tvos","android","android_tv"])
        for k in ["app_version","build","os_version","device_model"] { try check(matches(try text(client[k]),"^[A-Za-z0-9._+-]{1,64}$")) }
        let capture=try object(m["capture"],["mode","window_ms","dropped_events","persistence"])
        try one(capture["mode"],["ordinary","reproduction"]);_ = try integer(capture["window_ms"],900000);try check(decimal(try text(capture["dropped_events"])));try one(capture["persistence"],["available","memory_only","unavailable"])
        guard let components=m["components"] as? [Any] else {throw Invalid.schema};try check(!components.isEmpty && components.count<=16)
        var seen=Set<String>()
        for raw in components {
            let c=try object(raw,["name","state"],["reason"]);try one(c["name"],["client","server","workers","connection","platform","clock"])
            try check(seen.insert(try text(c["name"])).inserted);try one(c["state"],["complete","partial","unavailable"])
            if try text(c["state"]) == "complete" {try check(c["reason"]==nil)} else {try one(c["reason"],reasons)}
        }
        try check(seen.contains("client"));let privacy=try object(m["privacy"],["titles_included","attachments_included"])
        for value in privacy.values { guard let n=value as? NSNumber else {throw Invalid.schema};try check(CFGetTypeID(n)==CFBooleanGetTypeID()) }
        guard let files=m["files"] as? [Any] else {throw Invalid.schema};try check(files.count>=3 && files.count<32)
        seen=[];var total:UInt64=0
        for raw in files {let f=try object(raw,["name","size_bytes","sha256"]);try one(f["name"],names);try check(seen.insert(try text(f["name"])).inserted);total += try integer(f["size_bytes"],8388608);try check(matches(try text(f["sha256"]),"^[0-9a-f]{64}$"))}
        try check(total<=67108864 && Set(["summary.txt","description.txt","client-events.jsonl"]).isSubset(of:seen) && (privacy["attachments_included"] as? Bool)==seen.contains("diagnostics/platform.json"))
        return m
    }
}
