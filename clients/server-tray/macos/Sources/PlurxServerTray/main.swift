// Plurx Server.app entry point: an accessory (LSUIElement) app with one
// status item per user session. A second launch (for example from Finder
// while the login item already started one) exits after activating nothing:
// the existing instance keeps the menu bar icon.
import AppKit
import PlurxTrayCore
import PlurxTrayUI

let bundleIdentifier = Bundle.main.bundleIdentifier ?? "tv.plurx.Server"
let me = ProcessInfo.processInfo.processIdentifier
let others = NSRunningApplication.runningApplications(withBundleIdentifier: bundleIdentifier)
    .filter { $0.processIdentifier != me }
if !others.isEmpty {
    FileHandle.standardError.write(Data("Plurx Server is already running (pid \(others[0].processIdentifier)).\n".utf8))
    exit(0)
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let delegate = MainActor.assumeIsolated {
    TrayApp(control: ControlRunner.bundled(), info: AppInfo.from(.main))
}
app.delegate = delegate
app.run()
