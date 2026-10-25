import CoreGraphics
import Foundation

// Independent OS observation, not a tray-provided or environment-provided claim.
guard let session = CGSessionCopyCurrentDictionary() as? [String: Any],
      let uid = session["kCGSSessionUserIDKey"] as? NSNumber,
      let onConsole = session["kCGSSessionOnConsoleKey"] as? Bool,
      let loginDone = session["kCGSessionLoginDoneKey"] as? Bool else {
    fputs("no observable console graphics session\n", stderr)
    exit(1)
}
let value: [String: Any] = [
    "uid": uid, "on_console": onConsole, "login_done": loginDone,
    "session_id": session["kCGSSessionIDKey"] ?? NSNull(),
]
let bytes = try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
FileHandle.standardOutput.write(bytes)
FileHandle.standardOutput.write(Data([10]))
