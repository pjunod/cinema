// Plurx Server.app entry point: an accessory (LSUIElement) app with one
// status item per user session. A second launch (for example from Finder
// while the login item already started one) exits after activating nothing:
// the existing instance keeps the menu bar icon.
import AppKit
import PlurxTrayCore
import PlurxTrayUI

let smoke: SmokeOptions?
do { smoke = try SmokeOptions.parse(Array(CommandLine.arguments.dropFirst())) } catch {
    fputs("invalid smoke arguments: \(error)\n", stderr)
    exit(2)
}

let bundleIdentifier = Bundle.main.bundleIdentifier ?? "tv.plurx.Server"
let me = ProcessInfo.processInfo.processIdentifier
let others = NSRunningApplication.runningApplications(withBundleIdentifier: bundleIdentifier)
    .filter { $0.processIdentifier != me }
if !others.isEmpty {
    if smoke != nil {
        fputs("smoke requires its own application instance\n", stderr)
        exit(1)
    }
    FileHandle.standardError.write(Data("Plurx Server is already running (pid \(others[0].processIdentifier)).\n".utf8))
    exit(0)
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let delegate = MainActor.assumeIsolated {
    let bundled = ControlRunner.bundled()
    let runner = ControlRunner(executable: bundled.executable, installationRecord: smoke?.installation)
    let info = AppInfo.from(.main)
    if let smoke {
        let recorded = SmokeControl(runner)
        let delegate = TrayApp(control: recorded, info: info)
        delegate.smokeDriver = SmokeDriver(options: smoke, control: recorded,
                                           controllerPath: runner.executable.path, info: info)
        return delegate
    }
    return TrayApp(control: runner, info: info)
}
app.delegate = delegate
app.run()

if smoke != nil { exit(MainActor.assumeIsolated { delegate.smokeDriver?.exitCode ?? 1 }) }
