//! The GTK4 control window (logs, authorization errors, confirmation and
//! upgrade progress) rendered from the same model entries as the tray menu.
//! Only GTK 4.0 API is used. Widgets post events; they never call the
//! controller or borrow the shell, so no handler re-enters the event loop.
use super::Event;
use crate::model::{self, Action, Check, Entry, Kind, Snapshot, TrayHost};
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use std::cell::RefCell;
use std::rc::Rc;

pub const TITLE: &str = "plurx server";

pub const NO_TRAY_HOST: &str = "No system tray is available in this desktop session (for example GNOME without a StatusNotifier/AppIndicator extension). This window provides the same controls. Closing it leaves the server running.";

fn post(events: &async_channel::Sender<Event>, event: Event) {
    let _ = events.try_send(event);
}

fn label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_wrap(true);
    label.set_xalign(0.0);
    label.set_selectable(true);
    label
}

pub struct ControlWindow {
    pub window: gtk::ApplicationWindow,
    content: gtk::Box,
}

impl ControlWindow {
    pub fn new(app: &gtk::Application, events: &async_channel::Sender<Event>) -> Self {
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title(TITLE)
            .default_width(440)
            .default_height(600)
            .build();
        window.set_hide_on_close(true);
        window.set_icon_name(Some(crate::APP_ID));
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.set_margin_top(14);
        content.set_margin_bottom(14);
        content.set_margin_start(14);
        content.set_margin_end(14);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_child(Some(&content));
        scroll.set_vexpand(true);
        window.set_child(Some(&scroll));
        let tx = events.clone();
        window.connect_close_request(move |_| {
            post(&tx, Event::WindowClosed);
            glib::Propagation::Proceed
        });
        Self { window, content }
    }

    pub fn render(
        &self,
        entries: &[Entry],
        snapshot: &Snapshot,
        events: &async_channel::Sender<Event>,
    ) {
        while let Some(child) = self.content.first_child() {
            self.content.remove(&child);
        }
        for (index, entry) in entries.iter().enumerate() {
            // The window is the destination of this item, not a control in it.
            if entry.action == Some(Action::ShowWindow) {
                continue;
            }
            let widget: gtk::Widget = match (entry.kind, &entry.action) {
                (Kind::Separator, _) => gtk::Separator::new(gtk::Orientation::Horizontal).upcast(),
                (Kind::Header, _) => {
                    let header = gtk::Label::new(None);
                    header.set_markup(&format!(
                        "<big><b>{}</b></big>",
                        glib::markup_escape_text(&entry.title)
                    ));
                    header.set_xalign(0.0);
                    header.upcast()
                }
                (Kind::Command, Some(action)) => match entry.check {
                    Some(check) => {
                        let button = gtk::CheckButton::with_label(&entry.title);
                        // State first, handler second: rendering never posts.
                        button.set_active(check == Check::On);
                        button.set_inconsistent(check == Check::Mixed);
                        button.set_sensitive(entry.enabled);
                        let (tx, action) = (events.clone(), action.clone());
                        button.connect_toggled(move |_| post(&tx, Event::Action(action.clone())));
                        button.upcast()
                    }
                    None => {
                        let button = gtk::Button::with_label(&entry.title);
                        button.set_sensitive(entry.enabled);
                        button.set_halign(gtk::Align::Start);
                        let (tx, action) = (events.clone(), action.clone());
                        button.connect_clicked(move |_| post(&tx, Event::Action(action.clone())));
                        button.upcast()
                    }
                },
                _ => {
                    let info = label(&entry.title);
                    info.add_css_class("dim-label");
                    info.upcast()
                }
            };
            self.content.append(&widget);
            if index == 0 && snapshot.tray_host == TrayHost::Absent {
                self.content.append(&label(NO_TRAY_HOST));
            }
            if index == 0 {
                self.render_upgrade(snapshot);
            }
        }
    }

    fn render_upgrade(&self, snapshot: &Snapshot) {
        let Some(update) = &snapshot.update else {
            return;
        };
        if !(update.state.is_in_flight() || update.state.is_terminal()) {
            return;
        }
        let title = gtk::Label::new(None);
        let version = update
            .version
            .as_deref()
            .map(|v| format!(" — {v}"))
            .unwrap_or_default();
        title.set_markup(&format!(
            "<b>{}</b>",
            glib::markup_escape_text(&format!("{}{version}", update.state.title()))
        ));
        title.set_xalign(0.0);
        self.content.append(&title);
        if let Some(fraction) = model::upgrade_progress(update.state) {
            let bar = gtk::ProgressBar::new();
            bar.set_fraction(fraction);
            self.content.append(&bar);
        }
        self.content
            .append(&label(&model::upgrade_explanation(Some(update))));
    }
}

/// Bounded log viewer: at most 1000 lines of the plurxd unit's journal, with
/// search, Copy and the truncation/permission state visible.
pub struct LogsWindow {
    window: gtk::Window,
    view: gtk::TextView,
    search: gtk::SearchEntry,
    note: gtk::Label,
    lines: Rc<RefCell<(Vec<String>, bool)>>,
}

impl LogsWindow {
    pub fn new(app: &gtk::Application, events: &async_channel::Sender<Event>) -> Self {
        let window = gtk::Window::builder()
            .title("plurx server log")
            .default_width(780)
            .default_height(480)
            .build();
        window.set_application(Some(app));
        window.set_hide_on_close(true);
        let view = gtk::TextView::new();
        view.set_editable(false);
        view.set_monospace(true);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_child(Some(&view));
        scroll.set_vexpand(true);
        let search = gtk::SearchEntry::new();
        search.set_hexpand(true);
        let refresh = gtk::Button::with_label("Refresh");
        let copy = gtk::Button::with_label("Copy");
        let note = gtk::Label::new(None);
        note.set_xalign(0.0);
        note.add_css_class("dim-label");
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.append(&search);
        bar.append(&refresh);
        bar.append(&copy);
        let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
        column.set_margin_top(10);
        column.set_margin_bottom(10);
        column.set_margin_start(10);
        column.set_margin_end(10);
        column.append(&bar);
        column.append(&scroll);
        column.append(&note);
        window.set_child(Some(&column));

        let lines = Rc::new(RefCell::new((Vec::new(), false)));
        let this = Self {
            window,
            view,
            search,
            note,
            lines,
        };
        {
            let (view, note, lines) =
                (this.view.clone(), this.note.clone(), Rc::clone(&this.lines));
            this.search.connect_search_changed(move |entry| {
                fill(&view, &note, &lines.borrow(), &entry.text());
            });
        }
        let tx = events.clone();
        refresh.connect_clicked(move |_| post(&tx, Event::Action(Action::ViewLogs)));
        let view = this.view.clone();
        copy.connect_clicked(move |button| {
            let buffer = view.buffer();
            let (start, end) = buffer.bounds();
            button
                .clipboard()
                .set_text(&buffer.text(&start, &end, false));
        });
        this
    }

    pub fn show(&self, lines: Vec<String>, truncated: bool) {
        *self.lines.borrow_mut() = (lines, truncated);
        fill(
            &self.view,
            &self.note,
            &self.lines.borrow(),
            &self.search.text(),
        );
        self.window.present();
    }
}

fn fill(view: &gtk::TextView, note: &gtk::Label, data: &(Vec<String>, bool), query: &str) {
    let shown = model::filter_logs(&data.0, query);
    view.buffer().set_text(&shown.join("\n"));
    let mut text = format!("{} of {} lines", shown.len(), data.0.len());
    if data.1 {
        text.push_str(&format!(
            " · older lines omitted (newest {} shown)",
            crate::command::MAX_LOG_LINES
        ));
    }
    note.set_text(&text);
}

/// A modal confirmation naming the interruption; the answer is posted.
pub fn confirm(
    parent: &gtk::ApplicationWindow,
    events: &async_channel::Sender<Event>,
    pending: crate::core::Pending,
    title: &str,
    message: &str,
    proceed: &str,
) {
    let dialog = gtk::MessageDialog::builder()
        .transient_for(parent)
        .modal(true)
        .message_type(gtk::MessageType::Warning)
        .buttons(gtk::ButtonsType::None)
        .text(title)
        .secondary_text(message)
        .build();
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button(proceed, gtk::ResponseType::Accept);
    dialog.set_default_response(gtk::ResponseType::Cancel);
    let tx = events.clone();
    let pending = RefCell::new(Some(pending));
    dialog.connect_response(move |dialog, response| {
        if let Some(pending) = pending.borrow_mut().take() {
            post(
                &tx,
                Event::Confirmed(pending, response == gtk::ResponseType::Accept),
            );
        }
        dialog.close();
    });
    dialog.present();
}
