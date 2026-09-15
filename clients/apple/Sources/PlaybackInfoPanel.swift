import SwiftUI

/// A player observation keeps its value, provenance and explanation together.
/// Finite and live players adapt their existing telemetry into this presentation.
struct PlaybackInfoFact: Identifiable {
    let id: String
    let label: String
    let value: String
    var note: String? = nil
    var group: String = "Picture & sound"
    var diagnosticOnly = false
}

struct PlaybackInfoPanel: View {
    let title: String
    let facts: [PlaybackInfoFact]
    @Binding var mode: PlaybackStatsMode
    var isLive = false
    let onClose: () -> Void
    @State private var expanded: Set<String> = ["Picture & sound"]
    #if os(tvOS)
    @FocusState private var closeFocused: Bool
    private let bodySize: CGFloat = 22
    private let labelSize: CGFloat = 18
    private let pictureSize: CGFloat = 48
    #else
    private let bodySize: CGFloat = 16
    private let labelSize: CGFloat = 13
    private let pictureSize: CGFloat = 36
    #endif
    private let groups = ["Picture & sound", "Buffer & delivery", "Live stream & reception", "Server work", "Session & history"]
    private let muted = Color.white.opacity(0.7)

    private func fact(_ id: String) -> PlaybackInfoFact? { facts.first { $0.id == id } }
    private func value(_ id: String) -> String { fact(id)?.value ?? "Not reported" }
    private var reason: String {
        if let reason = fact("reason") { return reason.value }
        let method = value("method").lowercased()
        if method.contains("cache") { return "Playing a prepared copy. Original and player picture sizes are reported separately." }
        if method.contains("direct") { return "Original media is delivered without server conversion." }
        if method.contains("remux") { return "Media is repackaged for this player; video is not re-encoded." }
        return "The server selected this delivery method. No further reason was reported."
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .top, spacing: 12) {
                VStack(alignment: .leading, spacing: 5) {
                    Text("Playback info").font(.system(size: bodySize + 6, weight: .semibold))
                    Text(title).font(.system(size: labelSize)).foregroundStyle(muted)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 0)
                Button { mode = mode == .mini ? .standard : .mini } label: {
                    Image(systemName: mode == .mini ? "arrow.up.left.and.arrow.down.right" : "minus")
                        .frame(minWidth: 44, minHeight: 44)
                }.accessibilityLabel(mode == .mini ? "Expand playback info" : "Compact playback info")
                closeButton
            }.padding(20)
            if mode != .mini {
                ViewThatFits(in: .horizontal) {
                    tabs(horizontal: true)
                    tabs(horizontal: false)
                }.padding(.horizontal, 16).padding(.bottom, 12)
                Divider().overlay(.white.opacity(0.12))
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 20) {
                    if mode == .mini {
                        adaptiveFacts {
                            summary("Playing resolution", value("decode_resolution"))
                            summary("Playback", value("player_state"))
                            summary("Buffered on device", value("client_loaded"))
                        }
                    } else if mode == .standard {
                        overview
                    } else {
                        Text("Source, stream and player observations are separate. Unavailable is not zero.")
                            .font(.system(size: labelSize)).foregroundStyle(muted)
                        ForEach(groups, id: \.self) { group in
                            let rows = facts.filter { $0.group == group && (mode == .debug || !$0.diagnosticOnly) }
                            if !rows.isEmpty { disclosure(group, rows: rows) }
                        }
                    }
                }.padding(20)
            }
        }
        .font(.system(size: bodySize))
        .foregroundStyle(.white)
        .background(Palette.playerChrome.opacity(0.97), in: RoundedRectangle(cornerRadius: 20))
        .overlay(RoundedRectangle(cornerRadius: 20).stroke(.white.opacity(0.15)))
        .clipShape(RoundedRectangle(cornerRadius: 20))
        #if os(tvOS)
        .onAppear { Task { @MainActor in await Task.yield(); closeFocused = true } }
        #endif
    }

    private var closeButton: some View {
        Button(action: onClose) {
            Image(systemName: "xmark").frame(minWidth: 44, minHeight: 44)
        }
        .accessibilityLabel("Close playback info")
        #if os(tvOS)
        .focused($closeFocused)
        #endif
    }

    private func tabs(horizontal: Bool) -> some View {
        let layout = horizontal ? AnyLayout(HStackLayout(spacing: 6)) : AnyLayout(VStackLayout(alignment: .leading, spacing: 6))
        return layout {
            ForEach([PlaybackStatsMode.standard, .details, .debug]) { candidate in
                Button { mode = candidate } label: {
                    Text(candidate.label)
                        .font(.system(size: labelSize + 1, weight: .semibold))
                        .padding(.horizontal, 12).frame(minHeight: 44)
                        .foregroundStyle(mode == candidate ? Palette.accent : muted)
                        .background(mode == candidate ? Palette.accent.opacity(0.16) : .clear, in: RoundedRectangle(cornerRadius: 9))
                }
                .accessibilityAddTraits(mode == candidate ? .isSelected : [])
            }
        }
    }

    private var overview: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text(value("player_state") + (isLive ? " · Live TV" : ""))
                .font(.system(size: bodySize, weight: .semibold)).foregroundStyle(Palette.accent)
            adaptiveFacts {
                summary("Playing resolution", value("decode_resolution"), note: "Size reported by the attached player.", size: pictureSize)
                summary(isLive ? "Broadcast source" : "Original file", value("source_resolution"), note: fact("source_video")?.value)
            }
            VStack(alignment: .leading, spacing: 6) {
                Text(value("method")).fontWeight(.semibold)
                Text(reason).font(.system(size: labelSize + 1)).foregroundStyle(muted)
            }.frame(maxWidth: .infinity, alignment: .leading).padding(16)
                .background(.white.opacity(0.055), in: RoundedRectangle(cornerRadius: 10))
            adaptiveFacts {
                summary("Audio track", fact("decode_audio")?.value ?? value("source_audio"), note: "Track metadata; device output is not reported.")
                summary("Subtitles", value("subtitles"), note: fact("subtitles")?.note)
            }
            Divider().overlay(.white.opacity(0.12))
            adaptiveFacts {
                summary("Buffered on this device", value("client_loaded"), note: "Contiguous media loaded ahead of your position.")
                summary(isLive ? "Behind stream live edge" : "Buffering interruptions", value(isLive ? "live_edge" : "stalls"), note: isLive ? "Behind the latest available media; not broadcast delay." : "Player-reported interruptions; intentional pauses excluded.")
            }
            if isLive { summary("Tuner reception", value("reception")) }
        }
        #if os(tvOS)
        .focusable()
        #endif
    }

    private func adaptiveFacts<Content: View>(@ViewBuilder content: () -> Content) -> some View {
        // A phone stacks; tablet and TV keep a clear source/player comparison.
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 250), spacing: 24, alignment: .topLeading)], alignment: .leading, spacing: 18, content: content)
    }

    private func summary(_ label: String, _ text: String, note: String? = nil, size: CGFloat? = nil) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(label).font(.system(size: labelSize)).foregroundStyle(muted)
            Text(text).font(.system(size: size ?? bodySize + 3, weight: .semibold)).monospacedDigit()
            if let note, !note.isEmpty { Text(note).font(.system(size: labelSize)).foregroundStyle(muted) }
        }.frame(maxWidth: .infinity, alignment: .leading).fixedSize(horizontal: false, vertical: true)
            .accessibilityElement(children: .combine)
    }

    private func disclosure(_ title: String, rows: [PlaybackInfoFact]) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Button {
                if expanded.contains(title) { expanded.remove(title) } else { expanded.insert(title) }
            } label: {
                HStack {
                    Text(title).fontWeight(.semibold)
                    Spacer()
                    Image(systemName: expanded.contains(title) ? "chevron.up" : "chevron.down")
                }.frame(minHeight: 48)
            }.accessibilityValue(expanded.contains(title) ? "Expanded" : "Collapsed")
            if expanded.contains(title) {
                ForEach(rows) { row in
                    VStack(alignment: .leading, spacing: 8) {
                        Divider().overlay(.white.opacity(0.12))
                        ViewThatFits(in: .horizontal) {
                            HStack(alignment: .top, spacing: 24) {
                                Text(row.label).foregroundStyle(muted)
                                Spacer(minLength: 16)
                                Text(row.value).monospacedDigit().multilineTextAlignment(.trailing)
                            }
                            VStack(alignment: .leading, spacing: 4) {
                                Text(row.label).foregroundStyle(muted)
                                Text(row.value).monospacedDigit()
                            }
                        }
                        if let note = row.note, !note.isEmpty {
                            Text(note).font(.system(size: labelSize)).foregroundStyle(muted)
                        }
                    }.padding(.vertical, 10).fixedSize(horizontal: false, vertical: true)
                        .accessibilityElement(children: .combine)
                        #if os(tvOS)
                        .focusable()
                        #endif
                }
            }
        }
    }
}

func playbackInfoGroup(_ section: String) -> String {
    switch section {
    case "SOURCE", "NOW DECODING": return "Picture & sound"
    case "BUFFERING / DELIVERY", "NETWORK": return "Buffer & delivery"
    case "SERVER": return "Server work"
    default: return "Session & history"
    }
}

func playbackInfoExplanation(_ id: String) -> String? {
    switch id {
    case "decode_resolution": return "Attached player measurement; never inferred from source size."
    case "source_resolution": return "Original file metadata."
    case "decode_audio": return "Selected stream track; not the device's audio output."
    case "client_loaded": return "Contiguous media loaded ahead on this device."
    case "server_ready": return "Complete media ahead on the server; separate from the device buffer."
    case "delivery_rate": return "Server-completed responses; not confirmed client receipt."
    case "observed_rate": return "Player measurement during transfers; bursty by design."
    case "stream_rate": return "Stream bitrate; not connection speed."
    case "delivered": return "Server-completed response bytes, not proof of playback."
    case "stalls": return "Player interruptions for this playback; intentional pauses excluded."
    case "status": return "Server work state; separate from whether the picture is playing."
    case "status_age": return "Age of the latest server status response."
    case "http_wait": return "Server responses waiting for publication; not player stalls."
    case "production_actual": return "Encoder progress ahead of demand; not loaded video."
    case "production_target": return "Pacing policy, not a measurement."
    default: return nil
    }
}
