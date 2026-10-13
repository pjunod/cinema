import Foundation

extension ReadAfterHTTP {
    /// API and control requests share this account-generation floor. Captured
    /// logout/foreign/profile requests cannot borrow it: the transport checks
    /// the already-authorized request against the atomic current credentials.
    static let shared = ReadAfterHTTP(authorization: {
        Session.shared.readAfterAuthorization
    }, recordPeers: { peers, captured in
        Session.shared.recordReadAfterPeers(peers, authorization: captured)
    })
}
