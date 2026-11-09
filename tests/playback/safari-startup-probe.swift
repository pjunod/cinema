// Native WebKit first-frame probe for scripts/safari-startup-acceptance.
// The HTML and controlled media are served only by the task-owned local server.
import AppKit
import Foundation
import WebKit

final class StartupReporter: NSObject, WKScriptMessageHandler {
    func userContentController(
        _ userContentController: WKUserContentController, didReceive message: WKScriptMessage
    ) {
        guard JSONSerialization.isValidJSONObject(message.body),
            let bytes = try? JSONSerialization.data(withJSONObject: message.body, options: [.sortedKeys]),
            let line = String(data: bytes, encoding: .utf8)
        else { return }
        print(line)
        fflush(stdout)
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let configuration = WKWebViewConfiguration()
configuration.mediaTypesRequiringUserActionForPlayback = []
let reporter = StartupReporter()
configuration.userContentController.add(reporter, name: "report")
let view = WKWebView(frame: NSRect(x: 0, y: 0, width: 640, height: 360), configuration: configuration)
let window = NSWindow(
    contentRect: NSRect(x: -10000, y: -10000, width: 640, height: 360),
    styleMask: [.borderless], backing: .buffered, defer: false)
window.contentView = view
window.orderFront(nil)
view.load(URLRequest(url: URL(string: CommandLine.arguments[1])!))
let until = Date().addingTimeInterval(Double(CommandLine.arguments[2])!)
while Date() < until {
    RunLoop.current.run(until: Date().addingTimeInterval(0.1))
}
// Query from the native observation clock. A stalled offscreen page can
// throttle JS timers; that must not erase the negative control's final state.
var sampled = false
view.evaluateJavaScript("report('complete')") { _, error in
    if let error { print("observation_error: \(error)") }
    sampled = true
}
let sampleDeadline = Date().addingTimeInterval(2)
while !sampled && Date() < sampleDeadline {
    RunLoop.current.run(until: Date().addingTimeInterval(0.05))
}
window.close()
