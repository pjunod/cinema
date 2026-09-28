import AVFoundation
import SwiftUI

/// Live TV keeps AVPlayer's in-band caption renderer. Choices come from the
/// attached playlist's actual legible group, never guessed service numbers.
@MainActor
final class LiveTvCaptions: ObservableObject {
    enum Selection: Equatable { case automatic, off, track(Int) }
    struct Choice: Identifiable { let id: Int; let label: String }
    @Published private(set) var choices: [Choice] = []
    @Published private(set) var selection: Selection = .automatic
    @Published private(set) var summary = "No live stream"
    private weak var item: AVPlayerItem?
    private weak var player: AVPlayer?
    private var group: AVMediaSelectionGroup?
    private var options: [AVMediaSelectionOption] = []
    private var discovery: Task<Void, Never>?
    private var observer: NSObjectProtocol?
    private var generation = 0
    private var captionsAdvertised = false

    func attach(item: AVPlayerItem, player: AVPlayer, captionsAdvertised: Bool) {
        detach()
        self.item = item
        self.player = player
        self.captionsAdvertised = captionsAdvertised
        player.appliesMediaSelectionCriteriaAutomatically = true
        summary = "Checking stream captions"
        let changed = makeSelectionObservation(item)
        observer = NotificationCenter.default.addObserver(
            forName: AVPlayerItem.mediaSelectionDidChangeNotification, object: item, queue: .main
        ) { _ in
            Task { @MainActor in _ = changed() }
        }
        refresh()
    }

    /// Public to the player's ready notification; metadata can arrive after
    /// attachment. Cancellation plus identity fences every async completion.
    func refresh() {
        guard discovery == nil, options.isEmpty, let item else { return }
        let expected = generation
        discovery = Task { @MainActor [weak self, weak item] in
            guard let self, let item else { return }
            defer { if self.owns(item, generation: expected) { self.discovery = nil } }
            for attempt in 0..<20 {
                let loaded = try? await item.asset.loadMediaSelectionGroup(for: .legible)
                guard !Task.isCancelled, self.owns(item, generation: expected) else { return }
                if let loaded {
                    self.group = loaded
                    self.options = loaded.options.filter {
                        Self.isSelectable(mediaType: $0.mediaType, captionsAdvertised: self.captionsAdvertised)
                    }
                    self.choices = self.options.enumerated().map { index, option in
                        Choice(id: index, label: option.displayName)
                    }
                    self.applySelection()
                    if !self.options.isEmpty || !self.captionsAdvertised { return }
                }
                self.updateSummary()
                if attempt < 19 { do { try await Task.sleep(nanoseconds: 500_000_000) } catch { return } }
            }
        }
    }

    /// A notification already queued for a predecessor cannot update its
    /// successor, including when the same item is reattached after stop.
    func makeSelectionObservation(_ item: AVPlayerItem) -> () -> Bool {
        let expected = generation
        return { [weak self, weak item] in
            guard let self, let item, self.owns(item, generation: expected) else { return false }
            self.updateSummary()
            return true
        }
    }

    private func owns(_ candidate: AVPlayerItem, generation expected: Int) -> Bool {
        generation == expected && item === candidate && player?.currentItem === candidate
    }

    /// AVFoundation can synthesize CC1 even when the master advertises no
    /// captions. The server's graph proof authorizes CC options; an actual
    /// subtitle rendition is independently authored by the playlist.
    static func isSelectable(mediaType: AVMediaType, captionsAdvertised: Bool) -> Bool {
        mediaType == .subtitle || (mediaType == .closedCaption && captionsAdvertised)
    }

    /// Menu actions are capabilities for their attachment, not ordinals that
    /// can accidentally select a different service on the next channel.
    func makeSelectionAction(_ choice: Selection) -> () -> Void {
        let expected = generation
        let attached = item
        return { [weak self, weak attached] in
            guard let self, let attached, self.owns(attached, generation: expected) else { return }
            self.select(choice)
        }
    }

    func select(_ choice: Selection) {
        if case .track(let index) = choice, !options.indices.contains(index) { return }
        selection = choice
        applySelection()
    }

    private func applySelection() {
        guard let item, player?.currentItem === item else { return }
        // Off takes effect even while asynchronous group discovery is pending.
        player?.appliesMediaSelectionCriteriaAutomatically = selection == .automatic
        if let group {
            if options.isEmpty {
                player?.appliesMediaSelectionCriteriaAutomatically = false
                item.select(nil, in: group)
                updateSummary()
                return
            }
            switch selection {
            case .automatic:
                player?.appliesMediaSelectionCriteriaAutomatically = true
                item.selectMediaOptionAutomatically(in: group)
            case .off:
                player?.appliesMediaSelectionCriteriaAutomatically = false
                item.select(nil, in: group)
            case .track(let index):
                guard options.indices.contains(index) else { return }
                player?.appliesMediaSelectionCriteriaAutomatically = false
                item.select(options[index], in: group)
            }
        }
        updateSummary()
    }

    private func updateSummary() {
        guard let item, player?.currentItem === item else { return }
        if selection == .off { summary = "Off"; return }
        guard let group, !choices.isEmpty else {
            summary = captionsAdvertised
                ? "Captions advertised; stream tracks unavailable"
                : "No captions advertised by this stream"
            return
        }
        let selected = item.currentMediaSelection.selectedMediaOption(in: group)?.displayName
        summary = selection == .automatic
            ? "Automatic · \(selected ?? "Off")" : (selected ?? "Off")
    }

    func detach() {
        generation += 1
        discovery?.cancel()
        discovery = nil
        if let observer { NotificationCenter.default.removeObserver(observer) }
        observer = nil
        item = nil
        player = nil
        group = nil
        options = []
        captionsAdvertised = false
        choices = []
        selection = .automatic
        summary = "No live stream"
    }
}

struct LiveTvCaptionMenu: View {
    @ObservedObject var captions: LiveTvCaptions
    var body: some View {
        Menu {
            Button(action: captions.makeSelectionAction(.automatic)) {
                Label("Automatic (system preference)", systemImage: captions.selection == .automatic ? "checkmark" : "captions.bubble")
            }
            .accessibilityIdentifier("live-tv-caption-automatic")
            Button(action: captions.makeSelectionAction(.off)) {
                Label("Off", systemImage: captions.selection == .off ? "checkmark" : "captions.bubble")
            }
            .accessibilityIdentifier("live-tv-caption-off")
            ForEach(captions.choices) { choice in
                Button(action: captions.makeSelectionAction(.track(choice.id))) {
                    Label(choice.label, systemImage: captions.selection == .track(choice.id) ? "checkmark" : "captions.bubble")
                }
                .accessibilityIdentifier("live-tv-caption-option-\(choice.id)")
            }
            if captions.choices.isEmpty { Text(captions.summary) }
        } label: {
            Label("Captions", systemImage: "captions.bubble")
        }
        .accessibilityIdentifier("live-tv-captions")
        .accessibilityValue(captions.summary)
        #if os(tvOS)
        .buttonStyle(TVReadableButtonStyle(prominent: false))
        .focusEffectDisabled()
        #endif
    }
}
