// Explicit, bounded qualification of the real menu-bar application. These
// switches are never inferred from environment variables or server responses.
import Foundation

public struct SmokeOptions: Equatable, Sendable {
    public enum RequestedAction: String, Sendable { case quit, stop, restart }
    public let receipt: URL
    public let action: RequestedAction
    public let timeoutSeconds: Int
    public let installation: URL?

    public enum Usage: Error { case invalid(String) }

    public static func parse(_ arguments: [String]) throws -> SmokeOptions? {
        if arguments.isEmpty { return nil }
        // LaunchServices can supply its process serial number on normal launch.
        if arguments.count == 1, arguments[0].hasPrefix("-psn_") { return nil }
        var values: [String: String] = [:]
        let flags: Set<String> = ["--smoke-receipt", "--smoke-action", "--smoke-timeout", "--smoke-installation"]
        var index = 0
        while index < arguments.count {
            let flag = arguments[index]
            guard flags.contains(flag), values[flag] == nil, index + 1 < arguments.count,
                  !arguments[index + 1].hasPrefix("--") else {
                throw Usage.invalid("unknown, duplicate, or incomplete smoke option: \(flag)")
            }
            values[flag] = arguments[index + 1]
            index += 2
        }
        func absolute(_ value: String) throws -> URL {
            guard value.hasPrefix("/"), !value.utf8.contains(0), value != "/",
                  !value.hasSuffix("/"), value.utf8.count <= 4096 else {
                throw Usage.invalid("smoke paths must be absolute file paths")
            }
            return URL(fileURLWithPath: value)
        }
        guard let receipt = values["--smoke-receipt"] else {
            throw Usage.invalid("smoke options require --smoke-receipt")
        }
        guard let action = RequestedAction(rawValue: values["--smoke-action"] ?? "quit") else {
            throw Usage.invalid("--smoke-action must be quit, stop, or restart")
        }
        let timeoutText = values["--smoke-timeout"] ?? "60"
        guard timeoutText.allSatisfy({ $0.isASCII && $0.isNumber }),
              let timeout = Int(timeoutText), (1...600).contains(timeout) else {
            throw Usage.invalid("--smoke-timeout must be an integer from 1 to 600")
        }
        return SmokeOptions(receipt: try absolute(receipt), action: action, timeoutSeconds: timeout,
                            installation: try values["--smoke-installation"].map(absolute))
    }
}

/// Observes the exact bundled CLI used by TrayController. It cannot add new
/// commands and does not change timeouts, installation identity, or responses.
public final class SmokeControl: ControlInvoking, @unchecked Sendable {
    private let base: ControlInvoking
    private let lock = NSLock()
    private var recorded: [ControlCommand] = []
    private var first: Result<ControlResult, ControlFailure>?

    public init(_ base: ControlInvoking) { self.base = base }
    public var commands: [ControlCommand] { lock.withLock { recorded } }
    public var firstStatus: Result<ControlResult, ControlFailure>? { lock.withLock { first } }

    public func run(_ command: ControlCommand) async -> Result<ControlResult, ControlFailure> {
        lock.withLock { recorded.append(command) }
        let result = await base.run(command)
        lock.withLock {
            if command == .status, first == nil { first = result }
        }
        return result
    }
}

/// The smoke driver uses the same confirmation and mutation path as a menu
/// selection. Unknown status or a disabled item never authorizes a mutation.
@MainActor
public func performSmokeAction(_ requested: SmokeOptions.RequestedAction,
                               controller: TrayController, control: SmokeControl) async -> Bool {
    if requested == .quit { return true } // Quit is performed last, by the normal action.
    let action: Action = requested == .stop ? .stop : .restart
    guard controller.snapshot.statusFailure == nil,
          buildMenu(controller.snapshot).contains(where: { $0.action == action && $0.enabled }) else {
        return false
    }
    let count = control.commands.count
    await controller.perform(action)
    let expected: ControlCommand = requested == .stop ? .stop : .restart
    let mutations = control.commands.dropFirst(count).filter(\.isMutation)
    return mutations == [expected] && controller.snapshot.lastError == nil && controller.snapshot.busy == nil
}
