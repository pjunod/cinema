import Foundation

extension ReadAfterHTTP {
    static func bindCaptured(origin: String, token: String?, generation: UInt64, to request: inout URLRequest) {
        let current = Session.shared.readAfterAuthorization
        let peers = current.generation == generation && current.origin == origin && current.token == token ? current.peers : []
        bind(.init(origin: origin, token: token, generation: generation, peers: peers), to: &request)
    }

    /// API and control requests share this account-generation floor. Captured
    /// logout/foreign/profile requests cannot borrow it: the transport checks
    /// the already-authorized request against the atomic current credentials.
    static let shared = ReadAfterHTTP(authorization: {
        Session.shared.readAfterAuthorization
    }, recordPeers: { peers, captured in
        Session.shared.recordReadAfterPeers(peers, authorization: captured)
    })
}
