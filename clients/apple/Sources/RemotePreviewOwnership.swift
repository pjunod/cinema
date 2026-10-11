/// A physical focus/press cancels a network-owned preview only. A physical
/// scrub, or a preview replaced by physical input, remains under its owner.
struct RemotePreviewOwnership {
    private var position: Int?
    mutating func claim(before: Int?, after: Int?) {
        if let after, before != after { position = after }
    }
    func owns(_ current: Int?) -> Bool { position != nil && current == position }
    mutating func clear() { position = nil }
    mutating func cancel(current: Int?) -> Int? {
        defer { clear() }
        return owns(current) ? nil : current
    }
}
