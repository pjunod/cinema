//! The StatusNotifierItem and its DBusMenu, rendered from the shared model.
//! ksni runs its own D-Bus thread; activations only post events to the GTK
//! loop and never call the controller here.
use super::Event;
use crate::model::{Action, Check, Entry, Kind};
use crate::protocol::ServiceState;

pub struct SniTray {
    pub entries: Vec<Entry>,
    pub state: ServiceState,
    pub events: async_channel::Sender<Event>,
}

/// DBusMenu treats `_` as a mnemonic marker.
fn label(text: &str) -> String {
    // Panel menus do not wrap. Keep controller errors and remote version text
    // from dictating the width of the desktop; the window retains full text.
    let single_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = single_line.chars();
    let mut title: String = chars.by_ref().take(56).collect();
    if chars.next().is_some() {
        title.push('…');
    }
    title.replace('_', "__")
}

fn post(events: &async_channel::Sender<Event>, event: Event) {
    let _ = events.try_send(event);
}

impl ksni::Tray for SniTray {
    fn id(&self) -> String {
        crate::APP_ID.into()
    }
    fn title(&self) -> String {
        "plurx".into()
    }
    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }
    fn icon_name(&self) -> String {
        self.state.icon_name()
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: format!("plurx server — {}", self.state.title()),
            ..Default::default()
        }
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        post(&self.events, Event::Action(Action::ShowWindow));
    }
    fn menu_about_to_show(&mut self) {
        post(&self.events, Event::MenuShown);
    }
    fn watcher_online(&self) {
        // Watcher ownership alone says nothing about a registered tray host.
        post(&self.events, Event::HostProbeTick);
    }
    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        post(&self.events, Event::WatcherOffline);
        // Keep the service: ksni re-registers when a watcher returns.
        true
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{CheckmarkItem, StandardItem};
        self.entries
            .iter()
            .filter(|entry| entry.kind != Kind::Detail)
            .map(|entry| match (entry.kind, &entry.action) {
                (Kind::Separator, _) => ksni::MenuItem::Separator,
                (Kind::Command, Some(action)) => {
                    let action: Action = action.clone();
                    let activate = Box::new(move |t: &mut Self| {
                        post(&t.events, Event::Action(action.clone()));
                    });
                    match entry.check {
                        Some(check) => CheckmarkItem {
                            label: label(&entry.title),
                            enabled: entry.enabled,
                            checked: check == Check::On,
                            activate,
                            ..Default::default()
                        }
                        .into(),
                        None => StandardItem {
                            label: label(&entry.title),
                            enabled: entry.enabled,
                            activate,
                            ..Default::default()
                        }
                        .into(),
                    }
                }
                _ => StandardItem {
                    label: label(&entry.title),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test assertions")]
    use super::*;
    use ksni::blocking::TrayMethods;
    use std::collections::HashMap;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::OwnedValue;

    const DOCKER_DETAIL: &str = "Docker restart policy: unless-stopped. Docker itself must start at boot; manually stopped containers remain stopped across Docker restarts.";

    fn running_status() -> crate::protocol::StatusV1 {
        crate::protocol::StatusV1 {
            installation_id: "test".into(),
            service_state: ServiceState::Running,
            version: Some("test".into()),
            browser_origin: "http://127.0.0.1:32400".into(),
            server_startup: crate::protocol::StartupStateV1 {
                state: crate::protocol::StartupState::Enabled,
                detail: Some(DOCKER_DETAIL.into()),
            },
            tray_startup: crate::protocol::StartupStateV1 {
                state: crate::protocol::StartupState::Enabled,
                detail: None,
            },
            transaction_id: None,
        }
    }

    #[test]
    fn panel_menu_is_compact_and_control_window_retains_details() {
        let mut snapshot = crate::model::Snapshot::new("test", crate::model::Management::Supported);
        snapshot.status = Some(running_status());
        // Long errors remain available in the wrapping window.
        let error = format!("Controller error\n{}", "é".repeat(100));
        snapshot.last_error = Some(error.clone());
        let entries = crate::model::build(&snapshot);
        assert!(entries
            .iter()
            .any(|e| e.title == DOCKER_DETAIL && e.kind == Kind::Detail));
        assert!(entries.iter().any(|e| e.title == error));
        let (tx, _rx) = async_channel::unbounded();
        let tray = SniTray {
            entries: entries.clone(),
            state: ServiceState::Unknown,
            events: tx,
        };
        let menu = ksni::Tray::menu(&tray);
        assert_eq!(menu.len(), entries.len() - 1);
        assert_eq!(label("hello_world\nnext"), "hello__world next");
        assert_eq!(label(&"é".repeat(100)).chars().count(), 57);
        assert!(label(&error).ends_with('…'));
        for item in menu {
            if let ksni::MenuItem::Standard(item) = item {
                assert!(!item.label.starts_with("Docker restart policy"));
                assert!(!item.label.contains('\n'));
                assert!(item.label.chars().count() <= 57);
            }
        }
    }

    #[test]
    #[allow(
        clippy::disallowed_methods,
        reason = "test-only private D-Bus session, independent of the daemon and its activity ledger"
    )]
    fn dbus_menu_clicks_reach_the_shell_after_refresh() {
        const CHILD: &str = "PLURX_MENU_TEST_BUS";
        if std::env::var_os(CHILD).is_none() {
            let mut child = Command::new("dbus-run-session")
                .arg("--")
                .arg(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "linux::tray::tests::dbus_menu_clicks_reach_the_shell_after_refresh",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success());
                    return;
                }
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("D-Bus menu click/refresh stalled");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let (tx, rx) = async_channel::unbounded();
        let mut snapshot = crate::model::Snapshot::new("test", crate::model::Management::Supported);
        let handle = SniTray {
            entries: crate::model::build(&snapshot),
            state: ServiceState::Unknown,
            events: tx,
        }
        .assume_sni_available(true)
        .spawn()
        .unwrap();
        let conn = Connection::session().unwrap();
        let bus = Proxy::new(
            &conn,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .unwrap();
        let names: Vec<String> = bus.call("ListNames", &()).unwrap();
        let name = names
            .into_iter()
            .find(|n| {
                n.starts_with(&format!(
                    "org.kde.StatusNotifierItem-{}-",
                    std::process::id()
                ))
            })
            .unwrap();
        let menu = Proxy::new(&conn, name.as_str(), "/MenuBar", "com.canonical.dbusmenu").unwrap();
        // Opening the panel menu and a successful status refresh must leave its callbacks live.
        let _: bool = menu.call("AboutToShow", &(0_i32,)).unwrap();
        snapshot.status = Some(running_status());
        let entries = crate::model::build(&snapshot);
        handle.update(|t| {
            t.entries = entries.clone();
            t.state = ServiceState::Running;
        });
        while rx.try_recv().is_ok() {}
        #[derive(serde::Deserialize, zbus::zvariant::Type)]
        struct Layout {
            id: i32,
            properties: HashMap<String, OwnedValue>,
            children: Vec<OwnedValue>,
        }
        for round in 0..2 {
            let items: Vec<(i32, HashMap<String, OwnedValue>)> = menu
                .call(
                    "GetGroupProperties",
                    &(Vec::<i32>::new(), Vec::<String>::new()),
                )
                .unwrap();
            for entry in entries.iter().filter(|e| e.enabled && e.action.is_some()) {
                let id = items
                    .iter()
                    .find(|(_, p)| {
                        p.get("label").is_some_and(|v| {
                            <&str>::try_from(v).ok() == Some(label(&entry.title).as_str())
                        })
                    })
                    .unwrap()
                    .0;
                let (_, layout): (u32, Layout) = menu
                    .call("GetLayout", &(id, 0_i32, Vec::<String>::new()))
                    .unwrap();
                assert_eq!(layout.id, id);
                assert!(layout.properties.contains_key("label"));
                assert!(layout.children.is_empty());
                if round == 0 {
                    let _: () = menu
                        .call("Event", &(id, "clicked", OwnedValue::from(0_i32), 0_u32))
                        .unwrap();
                } else {
                    let missing: Vec<i32> = menu
                        .call(
                            "EventGroup",
                            &(vec![(id, "clicked", OwnedValue::from(0_i32), 0_u32)],),
                        )
                        .unwrap();
                    assert!(missing.is_empty());
                }
                let event = rx.recv_blocking().unwrap();
                assert!(
                    matches!(event, Event::Action(ref a) if Some(a) == entry.action.as_ref()),
                    "wrong action for {}",
                    entry.title
                );
            }
            // A changing controller error inserts a new row and invalidates the
            // old layout again; fetching the new IDs must still work.
            snapshot.last_error = Some("An observed controller error".into());
            handle.update(|t| t.entries = crate::model::build(&snapshot));
        }
        handle.shutdown().wait();
    }
}
