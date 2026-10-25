// Runs the bundled plurx-control executable: one argument array per call, a
// cleaned environment, bounded stdout (the one JSON response) and bounded
// stderr (diagnostics, shown only on failure). Calls are asynchronous and
// never execute on the main thread, so AppKit stays responsive while a Stop
// waits for the daemon's drain receipt.
import Foundation

public protocol ControlInvoking: Sendable {
    func run(_ command: ControlCommand) async -> Result<ControlResult, ControlFailure>
}

public struct ControlRunner: ControlInvoking {
    public let executable: URL
    public let installationRecord: URL?
    /// Diagnostics beyond this are dropped; stderr is for humans, not data.
    public static let maximumDiagnosticBytes = 64 * 1024

    public init(executable: URL, installationRecord: URL? = nil) {
        self.executable = executable
        self.installationRecord = installationRecord
    }

    /// The controller copied into this app bundle beside the main executable.
    /// The app never searches PATH for it.
    public static func bundled(in bundle: Bundle = .main) -> ControlRunner {
        let url = bundle.bundleURL.appendingPathComponent("Contents/MacOS/plurx-control")
        return ControlRunner(executable: url)
    }

    public func argv(for command: ControlCommand) throws -> [String] {
        var arguments: [String] = []
        if let installationRecord {
            arguments += ["--installation", installationRecord.path]
        }
        return arguments + (try command.arguments())
    }

    /// The controller needs the user's home to find its per-user installation
    /// record; it must not inherit proxy settings or an injected library path
    /// (controller traffic disables proxy discovery by contract).
    public static func environment(from parent: [String: String]) -> [String: String] {
        var environment = ["PATH": "/usr/bin:/bin:/usr/sbin:/sbin"]
        for key in ["HOME", "USER", "LOGNAME", "TMPDIR", "LANG", "LC_ALL"] {
            if let value = parent[key] { environment[key] = value }
        }
        return environment
    }

    public func run(_ command: ControlCommand) async -> Result<ControlResult, ControlFailure> {
        let arguments: [String]
        do { arguments = try argv(for: command) } catch {
            return .failure(.protocolViolation("refused invalid controller argument: \(error)"))
        }
        let executable = self.executable
        let timeout = command.timeoutSeconds
        // A timed-out read is terminated. A timed-out mutation is left to
        // finish: the controller owns that operation (and an upgrade's worker
        // is detached from it), so killing the CLI could only lose its result.
        let terminateOnTimeout = !command.isMutation
        return await withCheckedContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                continuation.resume(returning: Self.execute(
                    executable: executable, arguments: arguments, timeout: timeout,
                    terminateOnTimeout: terminateOnTimeout))
            }
        }
    }

    static func execute(executable: URL, arguments: [String], timeout: Int,
                        terminateOnTimeout: Bool) -> Result<ControlResult, ControlFailure> {
        let process = Process()
        process.executableURL = executable
        process.arguments = arguments
        process.environment = environment(from: ProcessInfo.processInfo.environment)
        process.standardInput = FileHandle.nullDevice
        let stdout = Pipe()
        let stderr = Pipe()
        process.standardOutput = stdout
        process.standardError = stderr

        let output = BoundedReader(handle: stdout.fileHandleForReading, limit: maximumResponseBytes)
        let diagnostics = BoundedReader(handle: stderr.fileHandleForReading, limit: maximumDiagnosticBytes)
        let exited = DispatchSemaphore(value: 0)
        process.terminationHandler = { _ in exited.signal() }
        do {
            try process.run()
        } catch {
            return .failure(.launch("could not start \(executable.path): \(error.localizedDescription)"))
        }
        output.start()
        diagnostics.start()
        if exited.wait(timeout: .now() + .seconds(timeout)) == .timedOut {
            if terminateOnTimeout { process.terminate() }
            return .failure(.timedOut(seconds: timeout))
        }
        let data = output.finish()
        let stderrText = String(decoding: diagnostics.finish(), as: UTF8.self)
        if output.overflowed { return .failure(.outputTooLarge) }
        let decoded = decodeControlResponse(data)
        switch decoded {
        case .success:
            // The contract pairs success with exit status zero; anything else
            // is a broken controller, not a success to act on.
            guard process.terminationStatus == 0 else {
                return .failure(.exited(status: process.terminationStatus, diagnostics: stderrText))
            }
            return decoded
        case .failure(.controller):
            return decoded
        case .failure:
            if data.isEmpty || process.terminationStatus != 0 {
                return .failure(.exited(status: process.terminationStatus, diagnostics: stderrText))
            }
            return decoded
        }
    }
}

/// Drains a pipe on its own thread, keeping at most `limit` bytes and
/// discarding (while still reading, so the child never blocks) the rest.
final class BoundedReader: @unchecked Sendable {
    private let handle: FileHandle
    private let limit: Int
    private let lock = NSLock()
    private var buffer = Data()
    private var overflow = false
    private let done = DispatchSemaphore(value: 0)
    var overflowed: Bool { lock.withLock { overflow } }

    init(handle: FileHandle, limit: Int) {
        self.handle = handle
        self.limit = limit
    }

    func start() {
        Thread.detachNewThread { [self] in
            while true {
                let chunk = handle.availableData
                if chunk.isEmpty { break }
                lock.withLock {
                    let room = limit - buffer.count
                    if chunk.count > room {
                        overflow = true
                        if room > 0 { buffer.append(chunk.prefix(room)) }
                    } else {
                        buffer.append(chunk)
                    }
                }
            }
            done.signal()
        }
    }

    /// Called after the child exited. A descendant that inherited the pipe
    /// (which the controller's detached worker must not do) could hold it open
    /// indefinitely, so the wait is bounded and only bytes already read count.
    func finish(grace: DispatchTimeInterval = .seconds(2)) -> Data {
        if done.wait(timeout: .now() + grace) == .timedOut {
            try? handle.close()
            _ = done.wait(timeout: .now() + .milliseconds(200))
        }
        return lock.withLock { buffer }
    }
}
