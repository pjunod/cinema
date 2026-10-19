// Upgrade progress and result. The window follows the controller's
// transaction state; closing it (or quitting the app) does not affect the
// detached worker. Cancel is shown only while the state is cancellable
// (before the daemon commits to shutting down).
import AppKit
import PlurxTrayCore

/// The ordered phases a user sees; failures and recovery are terminal states
/// shown in place of the remaining phases.
public let upgradePhases: [UpdateState] = [.prepared, .waitingForIdle, .shutdownCommitted, .installing, .restarting, .complete]

/// Index of the phase reached, for a determinate progress bar.
public func upgradeProgress(_ state: UpdateState) -> Double? {
    guard let index = upgradePhases.firstIndex(of: state) else { return nil }
    return Double(index + 1) / Double(upgradePhases.count)
}

/// The explanatory line under the state, including the recovery guidance a
/// failed or interrupted transaction needs.
public func upgradeExplanation(_ update: UpdateResult?) -> String {
    guard let update else { return "No upgrade has been started." }
    let detail = update.detail.map { " \($0)" } ?? ""
    let version = update.version ?? "the selected version"
    switch update.state {
    case .prepared:
        return "plurx \(version) is downloaded and verified." + detail
    case .waitingForIdle:
        return "The server finishes current playback and recordings before it stops. You can still cancel." + detail
    case .shutdownCommitted, .installing, .restarting:
        return "The upgrade to \(version) is running and continues even if you quit this app." + detail
    case .complete:
        return "plurx \(version) is installed and running." + detail
    case .cancelled:
        return "The upgrade was cancelled; the installed version is unchanged." + detail
    case .failed:
        return "The upgrade did not complete. The previous version remains or was restored." + detail
    case .recoveryRequired:
        return "The upgrade was interrupted and needs recovery before the server can start. Recovery runs automatically at the next server start; if it cannot, run `plurx-control updates status` for the transaction and follow docs/features/DESKTOP-UPGRADE-TRANSACTION.md." + detail
    default:
        return update.state.title + "." + detail
    }
}

@MainActor
public final class UpgradeWindowController: NSWindowController {
    private let stateLabel = NSTextField(labelWithString: "")
    private let explanation = NSTextField(wrappingLabelWithString: "")
    private let progress = NSProgressIndicator()
    private let cancelButton = NSButton(title: "Cancel Upgrade", target: nil, action: nil)
    public var onCancel: (() -> Void)?
    public private(set) var cancelVisible = false

    public init() {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 460, height: 170),
                              styleMask: [.titled, .closable], backing: .buffered, defer: true)
        window.title = "plurx upgrade"
        window.isReleasedWhenClosed = false
        super.init(window: window)
        stateLabel.font = .boldSystemFont(ofSize: NSFont.systemFontSize)
        progress.isIndeterminate = false
        progress.minValue = 0
        progress.maxValue = 1
        progress.setAccessibilityLabel("Upgrade progress")
        cancelButton.target = self
        cancelButton.action = #selector(cancelPressed)
        let stack = NSStackView(views: [stateLabel, progress, explanation, cancelButton])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.edgeInsets = NSEdgeInsets(top: 16, left: 16, bottom: 16, right: 16)
        stack.translatesAutoresizingMaskIntoConstraints = false
        let content = NSView()
        content.addSubview(stack)
        window.contentView = content
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            stack.topAnchor.constraint(equalTo: content.topAnchor),
            stack.bottomAnchor.constraint(lessThanOrEqualTo: content.bottomAnchor),
            progress.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -32),
            explanation.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -32),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    public var stateText: String { stateLabel.stringValue }
    public var explanationText: String { explanation.stringValue }

    public func update(_ update: UpdateResult?) {
        let state = update?.state ?? .idle
        let version = update?.version.map { " \($0)" } ?? ""
        stateLabel.stringValue = "\(state.title)\(state.isTerminal || state == .idle ? "" : "…")" + (version.isEmpty ? "" : " —" + version)
        explanation.stringValue = upgradeExplanation(update)
        if let value = upgradeProgress(state) {
            progress.isHidden = false
            progress.doubleValue = value
        } else {
            progress.isHidden = state.isTerminal
            progress.doubleValue = 0
        }
        cancelVisible = state.isCancellable
        cancelButton.isHidden = !cancelVisible
    }

    public func present(_ update: UpdateResult?) {
        self.update(update)
        showWindow(nil)
        window?.center()
        NSApp.activate(ignoringOtherApps: true)
    }

    @objc private func cancelPressed() { onCancel?() }
}
