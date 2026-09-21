import AVKit
import SwiftUI

struct OpticalPlaybackContext: Hashable {
    let driveId: String
    let driveName: String
    let discId: String
    let mediaGeneration: String
    let titleId: String
    let title: String
    let angle: Int
    let startMs: Int
    let durationMs: Int?
    let audio: Int?
    let subtitle: Int?
}

struct OpticalHomeRow: View {
    let drives: [OpticalDriveDTO]

    private var inserted: [OpticalDriveDTO] { drives.filter { $0.disc != nil } }

    var body: some View {
        if !inserted.isEmpty {
            VStack(alignment: .leading, spacing: 12) {
                Text("Inserted disc")
                    .font(.headline.weight(.semibold))
                    .foregroundStyle(Palette.onBg)
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 14) {
                        ForEach(inserted) { drive in
                            NavigationLink(value: Route.opticalDrive(drive.id)) {
                                OpticalDriveCard(drive: drive)
                            }
                            .buttonStyle(.plain)
                        }
                    }
                }
            }
            .padding(.horizontal, screenHPad)
        }
    }
}

private struct OpticalDriveCard: View {
    let drive: OpticalDriveDTO

    var body: some View {
        HStack(spacing: 14) {
            Image(systemName: drive.disc?.format == "bluray" ? "opticaldisc.fill" : "opticaldisc")
                .font(.system(size: 42))
                .foregroundStyle(Palette.accent)
                .frame(width: 72, height: 96)
                .background(Palette.surface, in: RoundedRectangle(cornerRadius: 12))
            VStack(alignment: .leading, spacing: 6) {
                Text(drive.disc?.title ?? "Inserted disc")
                    .font(.headline).foregroundStyle(Palette.onBg).lineLimit(2)
                Text("\((drive.disc?.format ?? "disc").uppercased()) · \(drive.name)")
                    .font(.caption).foregroundStyle(Palette.muted)
                Label("Browse disc", systemImage: "play.fill")
                    .font(.caption.weight(.semibold)).foregroundStyle(Palette.accent)
            }
        }
        .padding(14)
        .frame(width: 350, alignment: .leading)
        .background(Palette.surface.opacity(0.75), in: RoundedRectangle(cornerRadius: 16))
    }
}

struct OpticalDiscView: View {
    @EnvironmentObject private var model: AppModel
    let driveId: String
    @State private var content: OpticalDriveDiscDTO?
    @State private var error: String?

    var body: some View {
        Group {
            if let content {
                ScrollView {
                    VStack(alignment: .leading, spacing: 22) {
                        VStack(alignment: .leading, spacing: 7) {
                            Text(content.drive.disc?.title ?? "Inserted disc")
                                .font(.largeTitle.bold()).foregroundStyle(Palette.onBg)
                            Text("\((content.drive.disc?.format ?? "disc").uppercased()) · \(content.drive.name)")
                                .foregroundStyle(Palette.muted)
                        }
                        ForEach(content.titles) { title in
                            if let disc = content.drive.disc {
                                NavigationLink(value: Route.opticalTitle(
                                    driveId: content.drive.id,
                                    driveName: content.drive.name,
                                    discId: disc.id,
                                    mediaGeneration: disc.mediaGeneration,
                                    titleId: title.id
                                )) {
                                    HStack {
                                        VStack(alignment: .leading, spacing: 5) {
                                            Text("Title \(title.id)").font(.headline)
                                            Text(opticalDuration(title.durationMs))
                                                .font(.caption).foregroundStyle(Palette.muted)
                                        }
                                        Spacer()
                                        Image(systemName: "chevron.right")
                                    }
                                    .padding(16)
                                    .background(Palette.surface, in: RoundedRectangle(cornerRadius: 12))
                                }
                                .buttonStyle(.plain)
                            }
                        }
                    }
                    .padding(screenHPad)
                }
            } else if let error {
                ContentUnavailableView("Disc unavailable", systemImage: "opticaldisc", description: Text(error))
            } else {
                ProgressView().tint(Palette.accent)
            }
        }
        .background(Palette.bg.ignoresSafeArea())
        .navigationTitle("Disc")
        .task { await load() }
    }

    private func load() async {
        do { content = try await model.requireAPI().opticalDrive(driveId) }
        catch { self.error = opticalErrorMessage(error) }
    }
}

struct OpticalTitleView: View {
    @EnvironmentObject private var model: AppModel
    let driveId: String
    let driveName: String
    let discId: String
    let mediaGeneration: String
    let titleId: String
    @State private var detail: OpticalTitleDetailDTO?
    @State private var selectedAudio: Int?
    @State private var selectedSubtitle: Int?
    @State private var error: String?

    var body: some View {
        Group {
            if let detail {
                ScrollView {
                    VStack(alignment: .leading, spacing: 20) {
                        Text(detail.disc.title).font(.largeTitle.bold()).foregroundStyle(Palette.onBg)
                        Text("Title \(titleId) · \(opticalDuration(detail.title.durationMs))")
                            .foregroundStyle(Palette.muted)
                        HStack(spacing: 12) {
                            playLink(detail: detail, startMs: detail.progress?.positionMs ?? 0,
                                     label: (detail.progress?.positionMs ?? 0) > 0 ? "Resume" : "Play")
                            if (detail.progress?.positionMs ?? 0) > 0 {
                                playLink(detail: detail, startMs: 0, label: "Play from start")
                            }
                        }
                        trackPickers(detail)
                        if !detail.chapters.isEmpty {
                            Text("Chapters").font(.title2.bold()).foregroundStyle(Palette.onBg)
                            ForEach(Array(detail.chapters.enumerated()), id: \.element.id) { index, chapter in
                                playLink(
                                    detail: detail,
                                    startMs: chapter.startMs ?? 0,
                                    label: "Chapter \(chapter.index ?? index + 1) · \(opticalDuration(chapter.startMs))"
                                )
                            }
                        }
                    }
                    .padding(screenHPad)
                }
            } else if let error {
                ContentUnavailableView("Title unavailable", systemImage: "exclamationmark.opticaldisc", description: Text(error))
            } else {
                ProgressView().tint(Palette.accent)
            }
        }
        .background(Palette.bg.ignoresSafeArea())
        .navigationTitle("Disc title")
        .task { await load() }
    }

    @ViewBuilder
    private func trackPickers(_ detail: OpticalTitleDetailDTO) -> some View {
        if !(detail.title.facts.audioStreams ?? []).isEmpty {
            Picker("Audio", selection: $selectedAudio) {
                Text("Default").tag(Int?.none)
                ForEach(detail.title.facts.audioStreams ?? []) { track in
                    Text(opticalTrackLabel(track)).tag(track.index)
                }
            }
        }
        if !(detail.title.facts.subtitleStreams ?? []).isEmpty {
            Picker("Subtitles", selection: $selectedSubtitle) {
                Text("Off").tag(Int?.none)
                ForEach(detail.title.facts.subtitleStreams ?? []) { track in
                    Text(opticalTrackLabel(track)).tag(track.index)
                }
            }
        }
    }

    private func playLink(detail: OpticalTitleDetailDTO, startMs: Int, label: String) -> some View {
        NavigationLink(value: Route.opticalPlayer(OpticalPlaybackContext(
            driveId: driveId,
            driveName: driveName,
            discId: discId,
            mediaGeneration: mediaGeneration,
            titleId: titleId,
            title: detail.disc.title,
            angle: 1,
            startMs: startMs,
            durationMs: detail.title.durationMs,
            audio: selectedAudio,
            subtitle: selectedSubtitle
        ))) {
            Label(label, systemImage: "play.fill")
                .padding(.horizontal, 18).frame(minHeight: 48)
                .background(Palette.accent, in: Capsule()).foregroundStyle(.white)
        }
        .buttonStyle(.plain)
    }

    private func load() async {
        do {
            detail = try await model.requireAPI().opticalTitle(discId: discId, titleId: titleId)
        } catch { self.error = opticalErrorMessage(error) }
    }
}

struct OpticalPlayerView: View {
    @EnvironmentObject private var model: AppModel
    let context: OpticalPlaybackContext
    @State private var player = AVPlayer()
    @State private var session: HlsStart?
    @State private var error: String?

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            if let error {
                ContentUnavailableView("Playback unavailable", systemImage: "exclamationmark.triangle", description: Text(error))
                    .foregroundStyle(.white)
            } else if session == nil {
                ProgressView("Preparing disc…").tint(.white).foregroundStyle(.white)
            } else {
                VideoPlayer(player: player).ignoresSafeArea()
            }
        }
        .navigationTitle(context.title)
        .task { await start() }
        .task(id: session?.sessionId) { await reportLoop() }
        .onDisappear { finish() }
    }

    private func start() async {
        let caps = model.caps()
        do {
            _ = try await model.requireAPI().opticalDecision(
                driveId: context.driveId,
                titleId: context.titleId,
                body: OpticalDecisionRequest(
                    expectedDiscId: context.discId,
                    mediaGeneration: context.mediaGeneration,
                    angle: context.angle,
                    caps: caps,
                    force: nil,
                    audio: context.audio,
                    subtitle: context.subtitle
                )
            )
            let started = try await model.requireAPI().createOpticalSession(
                driveId: context.driveId,
                titleId: context.titleId,
                body: OpticalSessionRequest(
                    expectedDiscId: context.discId,
                    mediaGeneration: context.mediaGeneration,
                    angle: context.angle,
                    playbackId: UUID().uuidString,
                    requestId: UUID().uuidString,
                    start: Double(context.startMs) / 1000,
                    height: nil,
                    audio: context.audio,
                    subtitleBurn: context.subtitle,
                    audioOffsetMs: nil,
                    blockBudgetSecs: nil,
                    caps: caps
                )
            )
            guard let url = Session.shared.url(started.playlistUrl) else { throw APIError.badURL }
            session = started
            player.replaceCurrentItem(with: AVPlayerItem(url: url))
            if context.startMs > 0 {
                _ = await player.seek(to: CMTime(value: CMTimeValue(context.startMs), timescale: 1000))
            }
            player.play()
        } catch { self.error = opticalErrorMessage(error) }
    }

    private func reportLoop() async {
        while !Task.isCancelled, session != nil {
            try? await Task.sleep(for: .seconds(5))
            await reportProgress()
        }
    }

    private func reportProgress() async {
        guard let session else { return }
        let seconds = player.currentTime().seconds
        guard seconds.isFinite && seconds >= 0 else { return }
        _ = try? await model.requireAPI().opticalProgress(
            discId: context.discId,
            titleId: context.titleId,
            body: OpticalProgressRequest(
                driveId: context.driveId,
                mediaGeneration: context.mediaGeneration,
                sessionId: session.sessionId,
                angle: context.angle,
                positionMs: Int(seconds * 1000),
                durationMs: context.durationMs,
                audio: context.audio,
                subtitle: context.subtitle,
                recordedAtMs: Int(Date().timeIntervalSince1970 * 1000)
            )
        )
    }

    private func finish() {
        player.pause()
        guard let session else { return }
        let seconds = player.currentTime().seconds
        let finalPositionMs = seconds.isFinite && seconds >= 0 ? Int(seconds * 1000) : 0
        Task {
            _ = try? await model.requireAPI().opticalProgress(
                discId: context.discId,
                titleId: context.titleId,
                body: OpticalProgressRequest(
                    driveId: context.driveId,
                    mediaGeneration: context.mediaGeneration,
                    sessionId: session.sessionId,
                    angle: context.angle,
                    positionMs: finalPositionMs,
                    durationMs: context.durationMs,
                    audio: context.audio,
                    subtitle: context.subtitle,
                    recordedAtMs: Int(Date().timeIntervalSince1970 * 1000)
                )
            )
            await model.requireAPI().endHlsSession(session.sessionId)
        }
        self.session = nil
    }
}

private func opticalDuration(_ milliseconds: Int?) -> String {
    guard let milliseconds, milliseconds > 0 else { return "Duration unavailable" }
    let minutes = milliseconds / 60_000
    return minutes >= 60 ? "\(minutes / 60)h \(minutes % 60)m" : "\(minutes)m"
}

private func opticalTrackLabel(_ track: OpticalTrackDTO) -> String {
    [track.language, track.title, track.channels.map { "\($0) ch" }, track.codec.uppercased()]
        .compactMap { $0 }.joined(separator: " · ")
}

private func opticalErrorMessage(_ error: Error) -> String {
    switch (error as? APIError)?.refusalCode {
    case "optical_media_changed": return "The disc changed. Return to the disc and choose the title again."
    case "optical_drive_busy": return "This drive is already in use."
    case "optical_owner_unavailable": return "The drive host is offline."
    case "optical_reader_unavailable", "optical_read_failed": return "The drive could not read this title."
    default: return (error as? LocalizedError)?.errorDescription ?? error.localizedDescription
    }
}
