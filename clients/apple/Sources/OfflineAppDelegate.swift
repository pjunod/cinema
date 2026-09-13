#if os(iOS)
import UIKit

final class OfflineAppDelegate: NSObject, UIApplicationDelegate {
    /// The reminder delegate is installed here rather than from a view,
    /// because an action tapped on a notification while the app was not
    /// running is delivered during launch — before any view exists to hear it.
    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        LocalReminders.shared.begin()
        return true
    }

    func application(
        _ application: UIApplication,
        handleEventsForBackgroundURLSession identifier: String,
        completionHandler: @escaping () -> Void
    ) {
        OfflineDownloadManager.shared.handleEvents(
            forBackgroundURLSession: identifier,
            completion: completionHandler
        )
    }
}
#endif
