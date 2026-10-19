//! The Linux shell: GTK4 application, StatusNotifierItem and worker threads.
//!
//! Everything runs on the GTK main loop except controller calls (one worker
//! thread per call; their results come back as events) and ksni's own D-Bus
//! thread (which only posts events). The `Core` decides; this module only
//! executes its effects, so the main loop never waits on the controller.
mod dbus;
mod tray;
mod window;

use crate::autostart;
use crate::cli::{Mode, Options, Smoke};
use crate::core::{self, Core, Effect, Pending, Purpose};
use crate::model::{self, Action, TrayHost};
use crate::protocol::{ControlFailure, ControlResult};
use crate::receipt::{self, LinuxSession, SmokeAction};
use crate::runner::{Control, Runner};
use crate::session::{self, Eligibility, SessionFacts};
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use ksni::blocking::TrayMethods;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Login wait for a tray host before exiting quietly (§4.2).
pub const LOGIN_HOST_WAIT: Duration = Duration::from_secs(30);

pub enum Event {
    Action(Action),
    Confirmed(Pending, bool),
    Completed(Purpose, Result<ControlResult, ControlFailure>),
    HostOnline(bool),
    MenuShown,
    WindowClosed,
    PollTick,
    SmokeStep,
}

/// What the smoke driver has observed so far.
#[derive(Default)]
struct SmokeState {
    started: Option<Instant>,
    first_status_done: bool,
    menu: Option<receipt::Menu>,
    first_status: receipt::FirstStatus,
    action_issued: bool,
    action_done: bool,
    action_ok: Option<bool>,
    action_error: Option<String>,
    after_poll_done: bool,
    unknown_not_stopped: bool,
    start_only_when_stopped: bool,
    window_title: Option<String>,
    window_present: bool,
}

struct Shell {
    core: Core,
    control: Arc<dyn Control>,
    events: async_channel::Sender<Event>,
    app: gtk::Application,
    window: window::ControlWindow,
    logs: Option<window::LogsWindow>,
    tray: Option<ksni::blocking::Handle<tray::SniTray>>,
    control_path: String,
    window_visible: bool,
    menu_visible_until: Option<Instant>,
    poll_source: Option<glib::SourceId>,
    controller_calls: u32,
    smoke: Option<(Smoke, SmokeState)>,
    facts: SessionFacts,
    eligibility: Eligibility,
}

pub fn run(options: Options) -> i32 {
    // Session eligibility first: nothing is created for a refused session.
    let facts = session::probe::gather();
    let eligibility = session::evaluate(&facts);
    let systemd = std::path::Path::new("/run/systemd/system").exists();
    match &options.mode {
        Mode::Autostart => {
            if !eligibility.eligible {
                eprintln!(
                    "plurx-tray: not starting automatically: {}",
                    eligibility.reason
                );
                return 0;
            }
            let preference = autostart::current();
            if !preference.launches() {
                eprintln!("plurx-tray: login start is off ({preference:?})");
                return 0;
            }
            let probe = dbus::wait_for_host(LOGIN_HOST_WAIT);
            if !probe.host_present() {
                eprintln!(
                    "plurx-tray: no system tray host appeared within {} seconds; use the plurx application launcher instead",
                    LOGIN_HOST_WAIT.as_secs()
                );
                return 0;
            }
        }
        Mode::Window => {
            // A launcher opened by hand still needs a display it can use; the
            // window explains a missing tray host itself.
            if facts.uid == 0 {
                eprintln!("plurx-tray: the desktop companion never runs as root");
                return 2;
            }
        }
        Mode::Smoke(smoke) => {
            if !eligibility.eligible {
                return write_refusal(smoke, &facts, &eligibility, &options);
            }
        }
    }

    let control: Arc<dyn Control> =
        Arc::new(Runner::new(&options.control, options.installation.clone()));
    let app = gtk::Application::builder()
        .application_id(crate::APP_ID)
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let smoke = match &options.mode {
        Mode::Smoke(s) => Some(s.clone()),
        _ => None,
    };
    let autostarted = options.mode == Mode::Autostart;
    let control_path = options.control.display().to_string();
    let exit = Rc::new(RefCell::new(0));
    let exit_in = Rc::clone(&exit);
    app.connect_activate(move |app| {
        if app.active_window().is_some() {
            return;
        }
        let (tx, rx) = async_channel::unbounded::<Event>();
        let management = core::management_for(systemd);
        let mut core = Core::new(crate::APP_VERSION, management, crate::release_repository());
        let probe = dbus::probe_host();
        core.snapshot.tray_host = if probe.host_present() {
            TrayHost::Present
        } else {
            TrayHost::Absent
        };
        let window = window::ControlWindow::new(app, &tx);
        let shell = Rc::new(RefCell::new(Shell {
            core,
            control: Arc::clone(&control),
            events: tx.clone(),
            app: app.clone(),
            window,
            logs: None,
            tray: None,
            control_path: control_path.clone(),
            window_visible: false,
            menu_visible_until: None,
            poll_source: None,
            controller_calls: 0,
            smoke: smoke.clone().map(|s| {
                (
                    s,
                    SmokeState {
                        started: Some(Instant::now()),
                        ..SmokeState::default()
                    },
                )
            }),
            facts: facts.clone(),
            eligibility: eligibility.clone(),
        }));
        // The application stays alive with no window while the tray exists.
        std::mem::forget(app.hold());
        {
            let mut s = shell.borrow_mut();
            s.spawn_tray();
            let show_window = !autostarted || s.tray.is_none();
            if show_window {
                s.show_window();
            }
            if s.tray.is_none() && autostarted {
                // Unreachable in practice (host proved above); never leave an
                // invisible process behind.
                app.quit();
                return;
            }
            let effects = s.core.poll();
            s.apply(effects);
        }
        let shell_rx = Rc::clone(&shell);
        let exit_rx = Rc::clone(&exit_in);
        glib::spawn_future_local(async move {
            while let Ok(event) = rx.recv().await {
                let code = shell_rx.borrow_mut().handle(event);
                if let Some(code) = code {
                    *exit_rx.borrow_mut() = code;
                    shell_rx.borrow_mut().shutdown();
                    break;
                }
            }
        });
        if smoke.is_some() {
            let tx = tx.clone();
            glib::timeout_add_local(Duration::from_millis(250), move || {
                if tx.try_send(Event::SmokeStep).is_err() {
                    return glib::ControlFlow::Break;
                }
                glib::ControlFlow::Continue
            });
        }
    });
    // GTK must not parse our own flags.
    app.run_with_args::<&str>(&[]);
    let code = *exit.borrow();
    code
}

fn write_refusal(
    smoke: &Smoke,
    facts: &SessionFacts,
    eligibility: &Eligibility,
    options: &Options,
) -> i32 {
    let value = receipt::Receipt {
        schema: receipt::SCHEMA,
        platform: "linux",
        app_version: crate::APP_VERSION.into(),
        app_build: crate::app_build().into(),
        arch: std::env::consts::ARCH,
        ui_pid: std::process::id(),
        uid: facts.uid,
        session: LinuxSession::from(facts, eligibility),
        tray: receipt::Tray::default(),
        window: Some(receipt::Window::default()),
        menu: receipt::Menu::default(),
        controller: receipt::Controller {
            path: options.control.display().to_string(),
            first_status: receipt::FirstStatus {
                failure: Some("not run: session ineligible".into()),
                ..receipt::FirstStatus::default()
            },
        },
        action: receipt::ActionResult {
            name: smoke.action,
            ok: None,
            service_state_after: None,
            error: None,
            ui_alive_after: false,
        },
        quit: receipt::Quit {
            via: "not_started",
            controller_calls_during_quit: 0,
        },
        invariants: receipt::Invariants {
            unknown_not_stopped: true,
            start_only_when_stopped: true,
            quit_sent_no_server_command: true,
            ui_alive_after_stop: None,
        },
        elapsed_ms: 0,
        incomplete: Some("session ineligible".into()),
    };
    match receipt::write_atomic(&smoke.receipt, &value) {
        Ok(()) => 3,
        Err(e) => {
            eprintln!("plurx-tray: could not write receipt: {e}");
            1
        }
    }
}

impl Shell {
    fn spawn_tray(&mut self) {
        let sni = tray::SniTray {
            entries: self.core.entries(),
            state: self.core.snapshot.service_state(),
            events: self.events.clone(),
        };
        // assume_sni_available: a missing watcher becomes watcher_offline (and
        // the window explains it) instead of a hard failure; ksni re-registers
        // when a watcher appears while we are alive.
        match sni.assume_sni_available(true).spawn() {
            Ok(handle) => self.tray = Some(handle),
            Err(e) => eprintln!("plurx-tray: tray item unavailable: {e}"),
        }
    }

    fn show_window(&mut self) {
        self.window_visible = true;
        self.render();
        self.window.window.present();
    }

    fn visible(&self) -> bool {
        self.window_visible || self.menu_visible_until.is_some_and(|t| Instant::now() < t)
    }

    fn render(&mut self) {
        let entries = self.core.entries();
        if let Some(tray) = &self.tray {
            let state = self.core.snapshot.service_state();
            let menu = entries.clone();
            tray.update(move |t| {
                t.entries = menu;
                t.state = state;
            });
        }
        if self.window_visible {
            self.window
                .render(&entries, &self.core.snapshot, &self.events);
        }
    }

    fn schedule_poll(&mut self) {
        if let Some(source) = self.poll_source.take() {
            source.remove();
        }
        let Some(seconds) = model::poll_interval(&self.core.snapshot, self.visible()) else {
            return;
        };
        let tx = self.events.clone();
        self.poll_source = Some(glib::timeout_add_local_once(
            Duration::from_secs(seconds),
            move || {
                let _ = tx.try_send(Event::PollTick);
            },
        ));
    }

    fn run_command(&mut self, purpose: Purpose, command: crate::command::Command) {
        self.controller_calls += 1;
        let control = Arc::clone(&self.control);
        let tx = self.events.clone();
        std::thread::spawn(move || {
            let result = control.run(&command);
            let _ = tx.send_blocking(Event::Completed(purpose, result));
        });
    }

    fn apply(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Run { purpose, command } => self.run_command(purpose, command),
                Effect::Confirm {
                    pending,
                    title,
                    message,
                    proceed,
                } => {
                    if self.smoke.is_some() {
                        // Smoke mode auto-accepts only the interruption it was asked to perform.
                        let _ = self.events.try_send(Event::Confirmed(pending, true));
                    } else {
                        if !self.window_visible {
                            self.show_window();
                        }
                        window::confirm(
                            &self.window.window,
                            &self.events,
                            pending,
                            &title,
                            &message,
                            &proceed,
                        );
                    }
                }
                Effect::OpenUrl(url) => {
                    if let Err(e) = gtk::gio::AppInfo::launch_default_for_uri(
                        &url,
                        None::<&gtk::gio::AppLaunchContext>,
                    ) {
                        self.show_error(&format!("Could not open {url}: {e}"));
                    }
                }
                Effect::ShowLogs { lines, truncated } => {
                    if self.logs.is_none() {
                        self.logs = Some(window::LogsWindow::new(&self.app, &self.events));
                    }
                    if let Some(logs) = &self.logs {
                        logs.show(lines, truncated);
                    }
                }
                Effect::ShowWindow | Effect::ShowUpgrade => self.show_window(),
                Effect::ShowError(message) => self.show_error(&message),
                Effect::Render => self.render(),
                Effect::Quit => {
                    let _ = self.events.try_send(Event::WindowClosed);
                    self.app.quit();
                }
            }
        }
        self.schedule_poll();
    }

    fn show_error(&mut self, message: &str) {
        if self.smoke.is_some() {
            return;
        }
        // The core already put the error in the model; the window shows it
        // next to the controls whose state it preserved.
        let _ = message;
        if !self.window_visible {
            self.show_window();
        }
    }

    fn shutdown(&mut self) {
        if let Some(tray) = self.tray.take() {
            tray.shutdown().wait();
        }
        self.app.quit();
    }

    /// Returns Some(exit code) when the shell should stop.
    fn handle(&mut self, event: Event) -> Option<i32> {
        match event {
            Event::Action(action) => {
                if action == Action::Quit && self.smoke.is_none() {
                    // Quit leaves the server alone: no controller call.
                    return Some(0);
                }
                let effects = self.core.act(action);
                self.apply(effects);
            }
            Event::Confirmed(pending, accepted) => {
                let effects = self.core.confirmed(pending, accepted);
                self.apply(effects);
            }
            Event::Completed(purpose, result) => {
                let poll = purpose == Purpose::Poll;
                let action_result =
                    matches!(purpose, Purpose::Lifecycle(_)).then(|| result.clone());
                let effects = self.core.completed(purpose, result);
                self.apply(effects);
                self.smoke_observe(poll, action_result);
            }
            Event::HostOnline(online) => {
                let host = if online {
                    TrayHost::Present
                } else {
                    TrayHost::Absent
                };
                let effects = self.core.set_tray_host(host);
                self.apply(effects);
            }
            Event::MenuShown => {
                self.menu_visible_until = Some(Instant::now() + Duration::from_secs(10));
                let effects = self.core.poll();
                self.apply(effects);
            }
            Event::WindowClosed => {
                self.window_visible = false;
                // With no tray host nothing remains visible: exit rather than
                // leave an invisible process behind. The server is untouched.
                if self.core.snapshot.tray_host == TrayHost::Absent && self.smoke.is_none() {
                    return Some(0);
                }
                self.schedule_poll();
            }
            Event::PollTick => {
                let effects = self.core.poll();
                self.apply(effects);
            }
            Event::SmokeStep => return self.smoke_step(),
        }
        None
    }

    fn smoke_observe(
        &mut self,
        poll: bool,
        action_result: Option<Result<ControlResult, ControlFailure>>,
    ) {
        let snapshot = self.core.snapshot.clone();
        let Some((_, state)) = self.smoke.as_mut() else {
            return;
        };
        if poll {
            let (unknown, start) = receipt::menu_invariants(&snapshot);
            if !state.first_status_done {
                state.first_status_done = true;
                state.first_status = receipt::FirstStatus::from(&snapshot);
                state.menu = Some(receipt::Menu::from(&model::build(&snapshot)));
                state.unknown_not_stopped = unknown;
                state.start_only_when_stopped = start;
            } else {
                state.unknown_not_stopped &= unknown;
                state.start_only_when_stopped &= start;
                if state.action_done {
                    state.after_poll_done = true;
                }
            }
        }
        if let Some(result) = action_result {
            state.action_done = true;
            state.action_ok = Some(result.is_ok());
            state.action_error = result.err().map(|f| f.message());
        }
    }

    fn smoke_step(&mut self) -> Option<i32> {
        let window_present = self.window.window.is_visible() && self.window.window.is_mapped();
        let window_title = self.window.window.title().map(|t| t.to_string());
        let (deadline_hit, action, ready_for_action, finished) = {
            let (smoke, state) = self.smoke.as_mut()?;
            state.window_present |= window_present;
            if window_present {
                state.window_title = window_title;
            }
            let elapsed = state.started.map(|s| s.elapsed()).unwrap_or_default();
            let action = smoke.action.action();
            let ready = state.first_status_done && state.window_present && !state.action_issued;
            let finished = state.first_status_done
                && state.window_present
                && (action.is_none() || (state.action_done && state.after_poll_done));
            (elapsed >= smoke.timeout, action, ready, finished)
        };
        if ready_for_action {
            if let Some((_, state)) = self.smoke.as_mut() {
                state.action_issued = true;
            }
            if let Some(action) = action {
                let enabled = model::find(&self.core.entries(), &action).is_some_and(|e| e.enabled);
                if enabled {
                    let effects = self.core.act(action);
                    self.apply(effects);
                } else if let Some((_, state)) = self.smoke.as_mut() {
                    state.action_done = true;
                    state.after_poll_done = true;
                    state.action_ok = Some(false);
                    state.action_error = Some(format!(
                        "{action:?} is not offered for an observed {} server",
                        self.core.snapshot.service_state().title()
                    ));
                }
            }
        }
        if finished || deadline_hit {
            return Some(self.finish_smoke(if finished {
                None
            } else {
                Some("timed out".into())
            }));
        }
        None
    }

    fn finish_smoke(&mut self, incomplete: Option<String>) -> i32 {
        // Quit through the normal action: it must not reach the controller.
        let calls_before = self.controller_calls;
        let quit_effects = self.core.act(Action::Quit);
        let quit_ok = quit_effects == vec![Effect::Quit];
        let calls_during_quit = self.controller_calls - calls_before;
        let probe = dbus::probe_host();
        let own = probe.own_item(std::process::id());
        let snapshot = self.core.snapshot.clone();
        let Some((smoke, state)) = self.smoke.take() else {
            return 1;
        };
        let stop_like = smoke.action != SmokeAction::Quit;
        let invariants = receipt::Invariants {
            unknown_not_stopped: state.unknown_not_stopped,
            start_only_when_stopped: state.start_only_when_stopped,
            quit_sent_no_server_command: quit_ok && calls_during_quit == 0,
            ui_alive_after_stop: stop_like.then_some(state.action_done),
        };
        let value = receipt::Receipt {
            schema: receipt::SCHEMA,
            platform: "linux",
            app_version: crate::APP_VERSION.into(),
            app_build: crate::app_build().into(),
            arch: std::env::consts::ARCH,
            ui_pid: std::process::id(),
            uid: self.facts.uid,
            session: LinuxSession::from(&self.facts, &self.eligibility),
            tray: receipt::Tray {
                watcher_present: probe.watcher_present,
                host_registered: probe.host_registered,
                item_registered: own.is_some(),
                item_service: own,
            },
            window: Some(receipt::Window {
                present: state.window_present,
                title: state.window_title.clone(),
            }),
            menu: state.menu.clone().unwrap_or_default(),
            controller: receipt::Controller {
                path: self.control_path.clone(),
                first_status: state.first_status.clone(),
            },
            action: receipt::ActionResult {
                name: smoke.action,
                ok: state.action_ok,
                service_state_after: stop_like.then(|| snapshot.service_state().wire().to_string()),
                error: state.action_error.clone(),
                ui_alive_after: true,
            },
            quit: receipt::Quit {
                via: "quit_action",
                controller_calls_during_quit: calls_during_quit,
            },
            elapsed_ms: state
                .started
                .map(|s| s.elapsed().as_millis())
                .unwrap_or_default(),
            incomplete: incomplete.clone(),
            invariants,
        };
        let pass = value.invariants.all_hold() && incomplete.is_none();
        match receipt::write_atomic(&smoke.receipt, &value) {
            Ok(()) if pass => 0,
            Ok(()) => 1,
            Err(e) => {
                eprintln!("plurx-tray: could not write receipt: {e}");
                1
            }
        }
    }
}
