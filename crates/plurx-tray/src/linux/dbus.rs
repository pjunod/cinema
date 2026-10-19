//! Session-bus probes for the StatusNotifier host. Blocking; called only
//! before the GTK loop starts or from worker threads.
use zbus::blocking::{Connection, Proxy};

pub const WATCHER: &str = "org.kde.StatusNotifierWatcher";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostProbe {
    /// A successful bus-name query distinguishes absence from an unavailable bus.
    pub watcher_observed: bool,
    pub watcher_present: bool,
    /// None when the watcher did not answer the property read.
    pub host_registered: Option<bool>,
    pub items: Vec<String>,
}

impl HostProbe {
    /// Automatic launch needs positive proof of a registered host.
    pub fn host_present(&self) -> bool {
        self.watcher_present && self.host_registered == Some(true)
    }

    /// Unknown queries cannot prove host presence or a confirmed host loss.
    pub fn state(&self) -> crate::model::TrayHost {
        use crate::model::TrayHost;
        if !self.watcher_observed {
            TrayHost::Unknown
        } else if !self.watcher_present {
            TrayHost::Absent
        } else {
            match self.host_registered {
                Some(true) => TrayHost::Present,
                Some(false) => TrayHost::Absent,
                None => TrayHost::Unknown,
            }
        }
    }

    /// The registered item belonging to this process. ksni registers the
    /// well-known name `org.kde.StatusNotifierItem-<pid>-<n>`.
    pub fn own_item(&self, pid: u32) -> Option<String> {
        let prefix = format!("org.kde.StatusNotifierItem-{pid}-");
        self.items.iter().find(|i| i.starts_with(&prefix)).cloned()
    }
}

pub fn probe_host() -> HostProbe {
    let mut probe = HostProbe::default();
    let Ok(conn) = Connection::session() else {
        return probe;
    };
    let Ok(bus) = Proxy::new(
        &conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    ) else {
        return probe;
    };
    let owned: Result<bool, _> = bus.call("NameHasOwner", &(WATCHER,));
    let Ok(owned) = owned else { return probe };
    probe.watcher_observed = true;
    probe.watcher_present = owned;
    if !owned {
        return probe;
    }
    if let Ok(watcher) = Proxy::new(&conn, WATCHER, "/StatusNotifierWatcher", WATCHER) {
        probe.host_registered = watcher
            .get_property::<bool>("IsStatusNotifierHostRegistered")
            .ok();
        probe.items = watcher
            .get_property::<Vec<String>>("RegisteredStatusNotifierItems")
            .unwrap_or_default();
    }
    probe
}

/// The login wait: at most `limit` for a tray host to appear.
pub fn wait_for_host(limit: std::time::Duration) -> HostProbe {
    let deadline = std::time::Instant::now() + limit;
    loop {
        let probe = probe_host();
        if probe.host_present() || std::time::Instant::now() >= deadline {
            return probe;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TrayHost;

    #[test]
    fn failed_queries_never_prove_a_host_and_confirmed_loss_is_distinct() {
        let mut probe = HostProbe::default();
        assert!(!probe.host_present());
        assert_eq!(probe.state(), TrayHost::Unknown);
        probe.watcher_observed = true;
        probe.watcher_present = true;
        assert!(!probe.host_present());
        assert_eq!(probe.state(), TrayHost::Unknown);
        probe.host_registered = Some(true);
        assert!(probe.host_present());
        assert_eq!(probe.state(), TrayHost::Present);
        // Registered-host loss is observable even while the watcher stays alive.
        probe.host_registered = Some(false);
        assert!(!probe.host_present());
        assert_eq!(probe.state(), TrayHost::Absent);
        probe.host_registered = Some(true);
        assert!(probe.host_present());
        // Full watcher loss overrides even a previously true host property.
        probe.watcher_present = false;
        assert!(!probe.host_present());
        assert_eq!(probe.state(), TrayHost::Absent);
        probe.watcher_present = true;
        assert_eq!(probe.state(), TrayHost::Present);
    }
}
