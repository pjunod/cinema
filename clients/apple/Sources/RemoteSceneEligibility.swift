import Foundation

struct RemoteSceneEligibility {
    private(set) var eligible = false
    private(set) var foregroundID = UUID()
    private var wasBackground = false
    mutating func transition(active: Bool, background: Bool) {
        if background { wasBackground = true }
        if active && wasBackground { foregroundID = UUID(); wasBackground = false }
        eligible = active
    }
}
