// Bounded log viewer: the controller returns at most 1000 lines of the
// LaunchAgent's actual log file as data; this window shows them with a search
// field and Copy, and says when older lines were cut.
import AppKit
import PlurxTrayCore

@MainActor
public final class LogWindowController: NSWindowController, NSSearchFieldDelegate {
    private var lines: [String] = []
    private var truncated = false
    private let textView = NSTextView()
    private let search = NSSearchField()
    private let note = NSTextField(labelWithString: "")
    public var onRefresh: (() -> Void)?

    public init() {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 760, height: 480),
                              styleMask: [.titled, .closable, .resizable, .miniaturizable],
                              backing: .buffered, defer: true)
        window.title = "plurx server log"
        window.isReleasedWhenClosed = false
        super.init(window: window)
        build(in: window)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    private func build(in window: NSWindow) {
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        textView.isEditable = false
        textView.isRichText = false
        textView.font = .monospacedSystemFont(ofSize: NSFont.smallSystemFontSize, weight: .regular)
        textView.isHorizontallyResizable = true
        textView.textContainer?.widthTracksTextView = false
        textView.textContainer?.containerSize = NSSize(width: CGFloat.greatestFiniteMagnitude,
                                                       height: CGFloat.greatestFiniteMagnitude)
        textView.setAccessibilityLabel("Server log lines")
        scroll.documentView = textView

        search.placeholderString = "Search log"
        search.delegate = self
        search.setAccessibilityLabel("Search the server log")
        let copy = NSButton(title: "Copy", target: self, action: #selector(copyShown))
        let refresh = NSButton(title: "Refresh", target: self, action: #selector(refreshPressed))
        note.textColor = .secondaryLabelColor

        let bar = NSStackView(views: [search, refresh, copy])
        bar.orientation = .horizontal
        let stack = NSStackView(views: [bar, scroll, note])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.edgeInsets = NSEdgeInsets(top: 10, left: 10, bottom: 10, right: 10)
        stack.translatesAutoresizingMaskIntoConstraints = false
        window.contentView = NSView()
        window.contentView?.addSubview(stack)
        if let content = window.contentView {
            NSLayoutConstraint.activate([
                stack.leadingAnchor.constraint(equalTo: content.leadingAnchor),
                stack.trailingAnchor.constraint(equalTo: content.trailingAnchor),
                stack.topAnchor.constraint(equalTo: content.topAnchor),
                stack.bottomAnchor.constraint(equalTo: content.bottomAnchor),
                bar.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -20),
                scroll.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -20),
            ])
        }
    }

    public func show(lines: [String], truncated: Bool) {
        self.lines = lines
        self.truncated = truncated
        render()
        showWindow(nil)
        window?.center()
        NSApp.activate(ignoringOtherApps: true)
    }

    public var shownText: String { textView.string }

    private func render() {
        let shown = filterLogLines(lines, query: search.stringValue)
        textView.string = shown.joined(separator: "\n")
        var parts = ["\(shown.count) of \(lines.count) lines"]
        if truncated { parts.append("older lines omitted (newest \(ControlCommand.maximumLogLines) shown)") }
        note.stringValue = parts.joined(separator: " · ")
        textView.scrollToEndOfDocument(nil)
    }

    public func controlTextDidChange(_ obj: Notification) { render() }

    @objc private func copyShown() {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(textView.string, forType: .string)
    }

    @objc private func refreshPressed() { onRefresh?() }
}
