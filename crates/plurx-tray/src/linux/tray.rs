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
    text.replace('_', "__")
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
