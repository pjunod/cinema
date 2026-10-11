import Foundation
import CoreFoundation

enum InvitationReadiness: String, Codable {
    case disabled, ready
    case globalDisabled = "global_disabled", loginChanged = "login_changed", grantRevoked = "grant_revoked"
    case permissionUnavailable = "permission_unavailable", providerUnconfigured = "provider_unconfigured"
    case transportPending = "transport_pending", transportUnavailable = "transport_unavailable"
    case retentionLimit = "retention_limit", migrationRemediation = "migration_remediation"
}
struct InvitationPhone: Codable, Equatable {
    let id: UUID
    let name: String
    let platform: String
    let generation: UInt64
    let createdAt: UInt64
    let permissionGranted: Bool
    let residentActive: Bool
}
struct InvitationConsent: Codable, Equatable {
    let receiverID: UUID
    let grantID: UUID?
    let enabled: Bool
    let transport: String?
    let generation: UInt64
    let transportGeneration: UInt64
    let eligible: Bool
    let readiness: InvitationReadiness
}
struct InvitationTicket: Equatable {
    let id: UUID
    let secret: String
    let expiresAt: UInt64
    let brokerOrigin: String
    let brokerGeneration: UUID
}
struct InvitationLookup: Equatable {
    let receiverID: UUID
    let foregroundID: UUID
    let target: CinemaRemoteTarget
    let expiresAt: UInt64
}
struct InvitationAPIError: Error, Equatable {
    let code: String
}

/// All production envelope decoders validate raw bounded JSON before constructing
/// DTOs. Codable on persisted metadata is never used to admit network envelopes.
enum InvitationWire {
    static let version = "cinema.invitation.v1"
    static let maximumBytes = 64 * 1_024
    static let maximumConsents = 160
    static func canonicalUUID(_ value: Any?) throws -> UUID {
        guard let text = value as? String, let id = UUID(uuidString: text),
              id.uuidString.lowercased() == text else { throw InvitationAPIError(code: "invalid") }
        return id
    }
    static func secret(_ value: Any?) throws -> String {
        guard let text = value as? String, text.utf8.count == 43,
              text.utf8.allSatisfy({ (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95 }),
              let bytes = Data(base64Encoded: text.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/") + "="),
              bytes.count == 32, base64URL(bytes) == text else { throw InvitationAPIError(code: "invalid") }
        return text
    }
    static func base64URL(_ data: Data) -> String {
        data.base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
    }
    static func opaqueID(_ value: String) -> UUID? {
        guard value.utf8.count == 43,
              value.utf8.allSatisfy({ (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95 }),
              let data = Data(base64Encoded: value.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/") + "="),
              data.count == 32, base64URL(data) == value else { return nil }
        let b = Array(data.prefix(16))
        return UUID(uuid: (b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]))
    }
    static func brokerOrigin(_ value: Any?) throws -> String {
        guard let text = value as? String, let url = URLComponents(string: text),
              url.scheme == "https", let host = url.host, !host.isEmpty,
              url.user == nil, url.password == nil, url.query == nil, url.fragment == nil,
              url.path.isEmpty, url.port.map({ (1...65535).contains($0) }) ?? true,
              url.url?.absoluteString == text else { throw InvitationAPIError(code: "invalid") }
        return text
    }
    static func object(_ value: Any?, keys: Set<String>) throws -> [String: Any] {
        guard let object = value as? [String: Any], Set(object.keys) == keys else { throw InvitationAPIError(code: "invalid") }
        return object
    }
    static func envelope(_ data: Data, keys: Set<String>) throws -> [String: Any] {
        guard data.count <= maximumBytes else { throw InvitationAPIError(code: "invalid") }
        try RemoteStrictJSON.check(data)
        let result = try object(JSONSerialization.jsonObject(with: data), keys: keys.union(["version"]))
        guard result["version"] as? String == version else { throw InvitationAPIError(code: "invalid") }
        return result
    }
    static func integer(_ value: Any?, zero: Bool = false) throws -> UInt64 {
        guard let number = value as? NSNumber, CFGetTypeID(number) != CFBooleanGetTypeID(),
              number.doubleValue >= (zero ? 0 : 1), number.doubleValue <= Double(CinemaRemoteCommand.maximumInteger) else { throw InvitationAPIError(code: "invalid") }
        return number.uint64Value
    }
    static func boolean(_ value: Any?) throws -> Bool {
        guard let number = value as? NSNumber, CFGetTypeID(number) == CFBooleanGetTypeID() else { throw InvitationAPIError(code: "invalid") }
        return number.boolValue
    }
    static func label(_ value: Any?, maximum: Int = 80) throws -> String {
        guard let text = value as? String, !text.isEmpty, text.utf8.count <= maximum,
              !text.unicodeScalars.contains(where: { $0.properties.generalCategory == .control }) else { throw InvitationAPIError(code: "invalid") }
        return text
    }
    static func phone(_ value: Any?) throws -> InvitationPhone {
        let o = try object(value, keys: ["installation_id", "name", "platform", "phone_generation", "created_at", "permission_granted", "resident_active"])
        guard let platform = o["platform"] as? String, ["apple", "android"].contains(platform) else { throw InvitationAPIError(code: "invalid") }
        let permission = try boolean(o["permission_granted"]), resident = try boolean(o["resident_active"])
        guard !resident || (platform == "android" && permission) else { throw InvitationAPIError(code: "invalid") }
        return try InvitationPhone(id: canonicalUUID(o["installation_id"]), name: label(o["name"]), platform: platform,
                                   generation: integer(o["phone_generation"]), createdAt: integer(o["created_at"], zero: true), permissionGranted: permission, residentActive: resident)
    }
    static func consent(_ value: Any?) throws -> InvitationConsent {
        let o = try object(value, keys: ["receiver_id", "grant_id", "enabled", "transport", "consent_generation", "transport_generation", "readiness"])
        let readiness = try object(o["readiness"], keys: ["eligible", "status", "provider_delivery_verified"])
        guard let status = readiness["status"] as? String, let state = InvitationReadiness(rawValue: status),
              try !boolean(readiness["provider_delivery_verified"]) else { throw InvitationAPIError(code: "invalid") }
        let grant = o["grant_id"] is NSNull ? nil : try canonicalUUID(o["grant_id"])
        let transport = o["transport"] is NSNull ? nil : o["transport"] as? String
        guard o["transport"] is NSNull || transport.map({ ["apns", "fcm", "android_resident"].contains($0) }) == true else { throw InvitationAPIError(code: "invalid") }
        let enabled = try boolean(o["enabled"]), eligible = try boolean(readiness["eligible"])
        let generation = try integer(o["consent_generation"], zero: true)
        let transportGeneration = try integer(o["transport_generation"], zero: true)
        guard eligible == (enabled && state == .ready),
              !enabled || (grant != nil && transport != nil && generation > 0),
              generation != 0 || (!enabled && grant == nil && transport == nil && transportGeneration == 0 && state == .disabled) else { throw InvitationAPIError(code: "invalid") }
        return try InvitationConsent(receiverID: canonicalUUID(o["receiver_id"]), grantID: grant, enabled: enabled, transport: transport,
                                     generation: generation, transportGeneration: transportGeneration, eligible: eligible, readiness: state)
    }
    static func registration(_ data: Data) throws -> (InvitationPhone, String?) {
        let o = try envelope(data, keys: ["phone", "phone_secret"])
        return (try phone(o["phone"]), o["phone_secret"] is NSNull ? nil : try secret(o["phone_secret"]))
    }
    static func phoneReply(_ data: Data) throws -> InvitationPhone { try phone(envelope(data, keys: ["phone"])["phone"]) }
    static func consentReply(_ data: Data) throws -> InvitationConsent { try consent(envelope(data, keys: ["consent"])["consent"]) }
    static func consentPage(_ data: Data) throws -> ([InvitationConsent], UUID?) {
        let o = try envelope(data, keys: ["consents", "next_cursor"])
        guard let values = o["consents"] as? [Any], values.count <= 20 else { throw InvitationAPIError(code: "invalid") }
        let rows = try values.map(consent)
        guard Set(rows.map(\.receiverID)).count == rows.count else { throw InvitationAPIError(code: "invalid") }
        return (rows, o["next_cursor"] is NSNull ? nil : try canonicalUUID(o["next_cursor"]))
    }
    static func transportStart(_ data: Data) throws -> (InvitationConsent, InvitationTicket?) {
        let o = try envelope(data, keys: ["consent", "ticket"]), c = try consent(o["consent"])
        guard !(o["ticket"] is NSNull) else { return (c, nil) }
        let t = try object(o["ticket"], keys: ["ticket_id", "ticket_secret", "expires_at", "broker_origin", "broker_generation"])
        return (c, try InvitationTicket(id: canonicalUUID(t["ticket_id"]), secret: secret(t["ticket_secret"]),
                                       expiresAt: integer(t["expires_at"]), brokerOrigin: brokerOrigin(t["broker_origin"]),
                                       brokerGeneration: canonicalUUID(t["broker_generation"])))
    }
    static func lookup(_ data: Data) throws -> InvitationLookup {
        let o = try envelope(data, keys: ["receiver_id", "foreground_id", "target", "expires_at"])
        let t = try object(o["target"], keys: ["owner_node_id", "session_id", "receiver_epoch"])
        let target = try CinemaRemoteTarget(ownerNodeID: label(t["owner_node_id"], maximum: 128), sessionID: canonicalUUID(t["session_id"]), receiverEpoch: canonicalUUID(t["receiver_epoch"]))
        return try InvitationLookup(receiverID: canonicalUUID(o["receiver_id"]), foregroundID: canonicalUUID(o["foreground_id"]), target: target, expiresAt: integer(o["expires_at"]))
    }
    static func phonePage(_ data: Data) throws -> ([InvitationPhone], UUID?) {
        let o = try envelope(data, keys: ["phones", "next_cursor"])
        guard let values = o["phones"] as? [Any], values.count <= 20 else { throw InvitationAPIError(code: "invalid") }
        let rows = try values.map(phone)
        guard Set(rows.map(\.id)).count == rows.count else { throw InvitationAPIError(code: "invalid") }
        let next = o["next_cursor"] is NSNull ? nil : try canonicalUUID(o["next_cursor"])
        return (rows, next)
    }
    static func deleted(_ data: Data) throws { _ = try envelope(data, keys: []) }
    static func error(_ data: Data) -> InvitationAPIError {
        guard let o = try? envelope(data, keys: ["code", "message"]),
              let code = try? label(o["code"], maximum: 80), (try? label(o["message"], maximum: 512)) != nil else { return .init(code: "unavailable") }
        return .init(code: code)
    }
    static func claimed(_ data: Data) throws {
        let o = try envelope(data, keys: ["status"])
        guard o["status"] as? String == "claimed" else { throw InvitationAPIError(code: "invalid") }
    }
}
