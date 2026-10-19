//! Session-bus probes for the StatusNotifier host. Blocking; called only
//! before the GTK loop starts or from worker threads.
use zbus::blocking::{Connection, Proxy};

pub const WATCHER: &str = "org.kde.StatusNotifierWatcher";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostProbe {
    pub watcher_present: bool,
    /// None when the watcher did not answer the property read.
    pub host_registered: Option<bool>,
    pub items: Vec<String>,
}

impl HostProbe {
    /// A watcher that exists and does not deny having a host. GNOME without
    /// an indicator extension has no watcher at all.
    pub fn host_present(&self) -> bool {
        self.watcher_present && self.host_registered != Some(false)
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
    if owned != Ok(true) {
        return probe;
    }
    probe.watcher_present = true;
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
