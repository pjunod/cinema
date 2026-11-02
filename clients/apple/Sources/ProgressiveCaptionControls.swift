import SwiftUI

struct ProgressiveCaptionControls: View {
    @ObservedObject var session: ProgressiveCaptionSession
    let request: (CaptionTask) -> Void
    let select: (CaptionAsset) -> Void
    @ObservedObject var textTranslation: SubtitleTextTranslation
    let useTextCaption: (TextCaptionResult) -> Void

    var body: some View {
        Form {
            TextCaptionTranslationControls(owner: textTranslation, use: useTextCaption)
            Section {
                Text(session.status).accessibilityIdentifier("caption-status")
                if !session.coverageStatus.isEmpty { Text(session.coverageStatus) }
                Text("Progressive captions appear on this device. To use Picture in Picture or AirPlay, choose Off or an original caption track first. Playback and saved enablement are independent.")
                    .font(.caption)
            } header: { Text("Progressive captions") }
            Section {
                Picker("Model and task", selection: $session.profileId) {
                    Text("Choose a model").tag("")
                    if !session.profileId.isEmpty, !session.profiles.contains(where: { $0.profileId == session.profileId }) { Text("Saved model · refresh status").tag(session.profileId) }
                    ForEach(session.profiles) { profile in Text("\(profile.label ?? profile.modelId) · \(profile.task.label)").tag(profile.profileId) }
                }
                Picker("Spoken language", selection: $session.spokenLanguage) {
                    Text("Choose spoken language").tag("")
                    if !session.spokenLanguage.isEmpty, !session.languages.contains(session.spokenLanguage) { Text("Saved language: " + session.spokenLanguage).tag(session.spokenLanguage) }
                    ForEach(session.languages, id: \.self) { language in
                        Text(Locale.current.localizedString(forLanguageCode: language) ?? language).tag(language)
                    }
                }
                ForEach(CaptionTask.allCases) { task in
                    Button(task.label) { request(task) }
                        .disabled(session.busy)
                        .accessibilityIdentifier("caption-request-" + task.rawValue)
                }
                if session.options?.permissions.demand != true {
                    Text("An administrator can request new work. You can select published captions below.").font(.caption)
                }
            } header: { Text("Generate while watching") }
            if !session.compatibleAssets.isEmpty || session.nextAssetCursor != nil {
                Section {
                    ForEach(session.compatibleAssets) { asset in
                        Button("\(asset.outputLanguage ?? asset.spokenLanguage) · \(asset.task?.label ?? "Published captions")") { select(asset) }
                    }
                    if session.nextAssetCursor != nil { Button("More published captions") { session.moreAssets() } }
                } header: { Text("Published captions") }
            }
            if session.selected || session.operation != nil {
                Section {
                    if session.canStop || session.stopRequested { Button(session.stopRequested ? "Retry Stop" : "Stop caption work") { session.stop() }.disabled(session.busy) }
                    if session.canRetry { Button("Retry caption work") { session.retry() }.disabled(session.busy) }
                    ForEach(session.failedRanges) { range in
                        if range.retryable {
                            Button("Retry range \(range.coreIndex + 1)") { session.retry(range) }.disabled(session.busy)
                        } else {
                            Text("Range \(range.coreIndex + 1): \(CaptionServiceError.message(range.reason))")
                        }
                    }
                    if session.nextFailureCursor != nil { Button("More failed ranges") { session.moreFailures() } }
                    HStack {
                        Button("Earlier 0.25 s") { session.subtitleOffsetMs = max(-10_000, session.subtitleOffsetMs - 250) }
                        Text(String(format: "%+.2f s", Double(session.subtitleOffsetMs) / 1000))
                        Button("Later 0.25 s") { session.subtitleOffsetMs = min(10_000, session.subtitleOffsetMs + 250) }
                    }
                    Button("Reset subtitle timing") { session.subtitleOffsetMs = 0 }
                    Text("Timing changes only the displayed captions. It never changes the source audio or generation ranges.").font(.caption)
                } header: { Text("Caption work and timing") }
            }
        }
        .navigationTitle("Caption tools")
    }
}
