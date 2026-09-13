#if os(iOS)
import Foundation
import UserNotifications

extension Notification.Name {
    /// A reminder's *Watch* action, carrying the channel it named. The Live TV
    /// page tunes it once its lineup has loaded.
    static let plurxReminderWatch = Notification.Name("plurx.reminderWatch")
}

/// Where a reminder's *Watch* action leaves its channel until Live TV is on
/// screen to take it.
///
/// The notification alone would not do: the tab that can tune a channel is
/// usually not built at the moment the action arrives, and a view that does
/// not exist hears nothing. A single string in the defaults, taken exactly
/// once, is the whole mechanism.
enum ReminderWatchRequest {
    private static let key = "plurx.reminderWatchChannel"

    static func set(_ channelId: String, defaults: UserDefaults = .standard) {
        defaults.set(channelId, forKey: key)
    }

    static func take(defaults: UserDefaults = .standard) -> String? {
        guard let channelId = defaults.string(forKey: key) else { return nil }
        defaults.removeObject(forKey: key)
        return channelId
    }
}

/// One local notification, as the plan decides it should exist.
struct LocalReminderRequest: Equatable, Sendable {
    let identifier: String
    let reminderId: String
    let channelId: String
    let title: String
    let body: String
    let fireAt: Int
}

/// The mirror, as arithmetic: what the phone should have pending, and what has
/// to change to get there. Kept apart from `UNUserNotificationCenter` so the
/// decision can be read — and tested — without a notification centre.
enum LocalReminderPlan {
    static let prefix = "plurx.reminder."

    /// The fire instant is part of the identifier on purpose. A reminder the
    /// server moved is then a *different* identifier, so "cancel what is no
    /// longer wanted, schedule what is missing" moves it, without this code
    /// ever having to read a pending request's trigger back and compare dates
    /// that the system stores in its own calendar representation.
    static func identifier(reminderId: String, fireAt: Int) -> String {
        "\(prefix)\(reminderId).\(fireAt)"
    }

    /// Only `armed` reminders in the future are mirrored. A fired one is the
    /// in-app overlay's business, and an expired or moved one is a row the
    /// server has already retired.
    static func requests(for reminders: [DvrReminder], now: Int) -> [LocalReminderRequest] {
        reminders
            .filter { $0.state == .armed && $0.fireAt > now }
            .map { reminder in
                LocalReminderRequest(
                    identifier: identifier(reminderId: reminder.id, fireAt: reminder.fireAt),
                    reminderId: reminder.id,
                    channelId: reminder.channelId,
                    title: reminder.title,
                    body: "\(reminder.guideNumber) · starts at \(liveTvTime(reminder.airingStart))",
                    fireAt: reminder.fireAt
                )
            }
    }

    /// A reconciliation, not an append: a reminder that is gone or has moved
    /// loses its pending request, and only the genuinely missing ones are
    /// scheduled. Identifiers this app did not write are left alone.
    static func reconcile(
        pending: [String], wanted: [LocalReminderRequest]
    ) -> (cancel: [String], schedule: [LocalReminderRequest]) {
        let mine = pending.filter { $0.hasPrefix(prefix) }
        let keep = Set(wanted.map(\.identifier))
        return (
            cancel: mine.filter { !keep.contains($0) },
            schedule: wanted.filter { !mine.contains($0.identifier) }
        )
    }
}

/// The server's armed reminders, mirrored into the phone's notification
/// centre.
///
/// The honest limitation: this is a mirror the phone refreshes when it is
/// awake. A reminder created, deleted or moved on another device — the
/// television, the web page, a second phone — while this phone stays closed is
/// not reflected until the app next opens. There is no push channel and no
/// background fetch behind this, so a notification can arrive for a reminder
/// somebody else cancelled an hour ago, and one set elsewhere this morning
/// will not ring here until the app is opened. Running the reconciliation on
/// launch and on every foreground is what keeps that window as short as an
/// app without a server push can make it.
final class LocalReminders: NSObject, UNUserNotificationCenterDelegate, @unchecked Sendable {
    static let shared = LocalReminders()

    static let category = "plurx.reminder"
    static let watchAction = "plurx.reminder.watch"
    static let recordAction = "plurx.reminder.record"

    private let center = UNUserNotificationCenter.current()

    /// Installed from the app delegate, so an action tapped while the app was
    /// not running is still delivered to something that can act on it.
    func begin() {
        center.delegate = self
        center.setNotificationCategories([UNNotificationCategory(
            identifier: Self.category,
            actions: [
                UNNotificationAction(identifier: Self.watchAction, title: "Watch",
                                     options: [.foreground]),
                UNNotificationAction(identifier: Self.recordAction, title: "Record",
                                     options: []),
            ],
            intentIdentifiers: [],
            options: []
        )])
    }

    /// Asked the first time a reminder is set, and never at launch: a
    /// permission sheet in front of someone who has not yet asked for a
    /// reminder is a sheet they say no to.
    @discardableResult
    func requestAuthorization() async -> Bool {
        let settings = await center.notificationSettings()
        switch settings.authorizationStatus {
        case .notDetermined:
            return (try? await center.requestAuthorization(options: [.alert, .sound])) ?? false
        case .denied:
            return false
        default:
            return true
        }
    }

    private func authorized() async -> Bool {
        switch await center.notificationSettings().authorizationStatus {
        case .authorized, .provisional, .ephemeral: return true
        default: return false
        }
    }

    /// Bring the phone's pending notifications into line with these rows.
    /// Silent when the viewer has not granted permission — this must never be
    /// the thing that asks.
    func mirror(_ reminders: [DvrReminder], now: Int = Int(Date().timeIntervalSince1970)) async {
        guard await authorized() else { return }
        let pending = await center.pendingNotificationRequests().map(\.identifier)
        let plan = LocalReminderPlan.reconcile(
            pending: pending,
            wanted: LocalReminderPlan.requests(for: reminders, now: now)
        )
        if !plan.cancel.isEmpty {
            center.removePendingNotificationRequests(withIdentifiers: plan.cancel)
        }
        for request in plan.schedule {
            try? await center.add(Self.notification(request))
        }
    }

    /// Launch and foreground. Reads the server's own list rather than trusting
    /// whatever this process last saw, because the point of the pass is to
    /// catch what changed while the app was closed.
    func reconcile(origin: String, token: String?) async {
        guard !origin.isEmpty, token != nil, await authorized() else { return }
        guard let reminders = try? await DvrAPI(origin: origin, token: token).reminders() else { return }
        await mirror(reminders)
    }

    private static func notification(_ request: LocalReminderRequest) -> UNNotificationRequest {
        let fire = Date(timeIntervalSince1970: TimeInterval(request.fireAt))
        // A calendar trigger rather than a time interval: an interval computed
        // now drifts by however long the device spends asleep before the
        // system schedules it, and a reminder that rings a minute late has
        // missed the thing it exists for.
        let components = Calendar.current.dateComponents(
            [.year, .month, .day, .hour, .minute, .second], from: fire)
        let content = UNMutableNotificationContent()
        content.title = request.title
        content.body = request.body
        content.sound = .default
        content.categoryIdentifier = LocalReminders.category
        content.userInfo = ["channel_id": request.channelId, "reminder_id": request.reminderId]
        return UNNotificationRequest(
            identifier: request.identifier,
            content: content,
            trigger: UNCalendarNotificationTrigger(dateMatching: components, repeats: false)
        )
    }

    // MARK: - UNUserNotificationCenterDelegate

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        [.banner, .sound]
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse
    ) async {
        let info = response.notification.request.content.userInfo
        guard let channelId = info["channel_id"] as? String else { return }
        switch response.actionIdentifier {
        case Self.recordAction:
            // The notification knows the channel; the airing is whatever the
            // guide says starts at the instant the reminder named, which is
            // the same identity the Record button uses.
            guard let reminderId = info["reminder_id"] as? String else { return }
            await record(channelId: channelId, reminderId: reminderId)
        case Self.watchAction, UNNotificationDefaultActionIdentifier:
            // Opening the notification itself means what Watch means. Swiping
            // it away does not, which is why the dismissal identifier falls
            // through to nothing rather than tuning a channel.
            ReminderWatchRequest.set(channelId)
            await MainActor.run {
                NotificationCenter.default.post(name: .plurxReminderWatch, object: nil)
            }
        default:
            break
        }
    }

    private func record(channelId: String, reminderId: String) async {
        let settings = SettingsStore()
        guard !settings.origin.isEmpty, let token = settings.token else { return }
        let api = DvrAPI(origin: settings.origin, token: token)
        guard let reminder = try? await api.reminders().first(where: { $0.id == reminderId })
        else { return }
        _ = try? await api.record(channelId: channelId, airingStart: reminder.airingStart)
        try? await api.ackReminder(reminderId)
    }
}
#endif
