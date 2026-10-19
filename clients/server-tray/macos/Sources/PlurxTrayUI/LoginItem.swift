// "Show tray app at login" is the app's own SMAppService.mainApp
// registration. Status is always read back from the OS; the app never
// registers itself at launch and never touches the server's LaunchAgent here
// (that is "Start server at login", owned by the controller).
import AppKit
import PlurxTrayCore
import ServiceManagement

@MainActor
public enum LoginItem {
    public static func status() -> TrayLoginStatus {
        map(SMAppService.mainApp.status)
    }

    public static func map(_ status: SMAppService.Status) -> TrayLoginStatus {
        switch status {
        case .enabled: return .enabled
        case .notRegistered: return .notRegistered
        case .requiresApproval: return .requiresApproval
        case .notFound: return .notFound
        @unknown default: return .unavailable("unrecognized login item state")
        }
    }

    public static func set(_ enabled: Bool) -> Result<TrayLoginStatus, LoginItemFailure> {
        do {
            if enabled {
                try SMAppService.mainApp.register()
            } else {
                try SMAppService.mainApp.unregister()
            }
        } catch {
            return .failure(LoginItemFailure(
                "The login item could not be \(enabled ? "added" : "removed"): \(error.localizedDescription)"))
        }
        return .success(status())
    }

    public static func openSettings() {
        SMAppService.openSystemSettingsLoginItems()
    }
}
