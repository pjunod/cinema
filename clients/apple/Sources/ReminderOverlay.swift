import SwiftUI

/// The in-app reminder: lower-left, three buttons, and a bar that fills as the
/// programme's start approaches.
///
/// Buttons only, deliberately. The ten-foot input contract decides what every
/// remote press means, and a panel that claimed a key for itself would be a
/// second answer to the same press — the surface would then behave differently
/// depending on whether a reminder happened to be showing. Back and Menu
/// dismiss this the way the platform dismisses anything else on the surface,
/// and nothing here touches the player's routing table.
struct ReminderOverlay: View {
    let reminder: DvrReminder
    let now: Int
    let watch: () -> Void
    let record: () -> Void
    /// Acknowledges it: gone here, and gone from every other device too.
    let dismiss: () -> Void
    /// The start arrived. Not an acknowledgement — the reminder has simply
    /// stopped being about something that is going to happen.
    let expire: () -> Void

    /// Zero at the moment the server fired it, one at the programme's start.
    private var countdown: Double {
        let span = reminder.airingStart - reminder.fireAt
        guard span > 0 else { return 1 }
        return min(max(Double(now - reminder.fireAt) / Double(span), 0), 1)
    }

    private var lead: String {
        let seconds = max(0, reminder.airingStart - now)
        if seconds < 60 { return "STARTS NOW" }
        return "STARTS IN \(seconds / 60) MIN"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(lead)
                .font(LiveTvType.eyebrow)
                .foregroundStyle(Palette.accent)
            Text(reminder.title)
                .font(LiveTvType.title)
                .lineLimit(1)
            Text("\(reminder.guideNumber) · \(liveTvTime(reminder.airingStart))"
                 + (reminder.covered ? " · already recording" : ""))
                .font(LiveTvType.secondary)
                .foregroundStyle(Palette.muted)
                .lineLimit(1)
            LiveTvProgressLine(value: countdown, height: 3)
            HStack(spacing: 8) {
                DvrActionButton(label: "Watch", prominent: true, action: watch)
                // Nothing to offer when a recording already covers the airing:
                // the button would either make a second row for one broadcast
                // or quietly do nothing at all.
                if !reminder.covered {
                    DvrActionButton(label: "Record", action: record)
                }
                DvrActionButton(label: "Dismiss", action: dismiss)
            }
        }
        .padding(reminderPadding)
        .frame(width: reminderWidth, alignment: .leading)
        .background(Palette.surface, in: RoundedRectangle(cornerRadius: 12))
        .overlay {
            RoundedRectangle(cornerRadius: 12).stroke(Palette.outline, lineWidth: 1)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Reminder: \(reminder.title) at \(liveTvTime(reminder.airingStart))")
        // The page's own thirty-second tick moves the bar; this exists only so
        // the panel leaves at the start rather than up to half a minute after
        // it, which is the one moment a countdown must not be approximate.
        .task(id: reminder.id) {
            let remaining = reminder.airingStart - Int(Date().timeIntervalSince1970)
            guard remaining > 0 else { return expire() }
            do { try await Task.sleep(nanoseconds: UInt64(remaining) * 1_000_000_000) }
            catch { return }
            guard !Task.isCancelled else { return }
            expire()
        }
    }

    #if os(tvOS)
    private var reminderWidth: CGFloat { 560 }
    private var reminderPadding: CGFloat { 24 }
    #else
    private var reminderWidth: CGFloat { 320 }
    private var reminderPadding: CGFloat { 14 }
    #endif
}
