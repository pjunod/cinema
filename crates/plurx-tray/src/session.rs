//! Whether this process may start the tray automatically
//! (DESKTOP-TRAY-IMPLEMENTATION.md §4.2). The decision is a pure function of
//! observed facts so every refusal is unit-tested; the Linux probe gathers
//! those facts from logind over the system bus and from the process
//! environment. Missing or ambiguous proof means no automatic launch: a
//! plugged-in monitor or a `DISPLAY` variable alone is never proof.
use serde::Serialize;

/// The logind session the tray would belong to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LogindSession {
    pub id: String,
    pub uid: u32,
    pub remote: bool,
    pub active: bool,
    /// `x11`, `wayland`, `tty`, `mir`, `unspecified`, …
    pub kind: String,
    /// `user`, `greeter`, `lock-screen`, `background`, …
    pub class: String,
    /// Seat id, empty when the session has none.
    pub seat: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SessionFacts {
    pub uid: u32,
    /// Container markers (/.dockerenv, /run/.containerenv,
    /// /run/systemd/container, `container=` in the environment).
    pub container: bool,
    /// SSH_CONNECTION / SSH_CLIENT / SSH_TTY present.
    pub ssh: bool,
    pub display: Option<String>,
    pub wayland_display: Option<String>,
    /// The Wayland socket exists under XDG_RUNTIME_DIR.
    pub wayland_socket: bool,
    /// The X11 display names a local socket (`:N` / `unix:N`) whose
    /// /tmp/.X11-unix/XN exists.
    pub x11_local_socket: bool,
    pub session_bus: bool,
    /// logind answered at all (system bus reachable, login1 present).
    pub logind: bool,
    /// Whether logind knows any seat on this machine.
    pub seats_exist: bool,
    pub session: Option<LogindSession>,
    /// How the session was found (XDG_SESSION_ID, PID, user Display).
    pub session_source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Eligibility {
    pub eligible: bool,
    pub reason: String,
}

fn no(reason: impl Into<String>) -> Eligibility {
    Eligibility {
        eligible: false,
        reason: reason.into(),
    }
}

/// True for an X11 display string that names a local server rather than a
/// TCP/forwarded one (`localhost:10.0`, `host:0`).
pub fn is_local_x11(display: &str) -> bool {
    let rest = display.strip_prefix("unix").unwrap_or(display);
    rest.strip_prefix(':')
        .and_then(|n| n.split('.').next())
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The X11 display number, for the socket check.
pub fn x11_display_number(display: &str) -> Option<&str> {
    if !is_local_x11(display) {
        return None;
    }
    let rest = display.strip_prefix("unix").unwrap_or(display);
    rest.strip_prefix(':').and_then(|n| n.split('.').next())
}

pub fn evaluate(f: &SessionFacts) -> Eligibility {
    if f.uid == 0 {
        return no("the desktop companion never runs as root");
    }
    if f.container {
        return no("running inside a container");
    }
    if !f.logind {
        return no("systemd-logind is unavailable, so the graphical session cannot be proved");
    }
    let Some(s) = &f.session else {
        return no("no logind session belongs to this process or user");
    };
    if s.uid != f.uid {
        return no(format!(
            "logind session {} belongs to uid {}, not {}",
            s.id, s.uid, f.uid
        ));
    }
    if s.remote {
        return no(format!("logind session {} is remote", s.id));
    }
    if !s.active {
        return no(format!("logind session {} is not the active session", s.id));
    }
    if s.class != "user" {
        return no(format!("logind session {} has class {}", s.id, s.class));
    }
    if s.kind != "x11" && s.kind != "wayland" {
        return no(format!(
            "logind session {} has type {}, not x11 or wayland",
            s.id,
            if s.kind.is_empty() {
                "unspecified"
            } else {
                &s.kind
            }
        ));
    }
    if f.seats_exist && s.seat.is_empty() {
        return no(format!("logind session {} has no local seat", s.id));
    }
    if f.ssh {
        return no("started from an SSH connection");
    }
    if !f.session_bus {
        return no("no usable session bus");
    }
    let usable_display = match s.kind.as_str() {
        "wayland" => f.wayland_display.is_some() && f.wayland_socket,
        _ => f.display.as_deref().is_some_and(is_local_x11) && f.x11_local_socket,
    };
    if !usable_display {
        return no(format!(
            "no usable local {} display in this process",
            s.kind
        ));
    }
    Eligibility {
        eligible: true,
        reason: format!(
            "local active {} session {} on seat {}",
            s.kind,
            s.id,
            if s.seat.is_empty() { "-" } else { &s.seat }
        ),
    }
}

#[cfg(target_os = "linux")]
pub mod probe {
    //! Fact gathering. Each logind query is bounded by zbus's own timeouts;
    //! failures become missing facts, which `evaluate` refuses.
    use super::{LogindSession, SessionFacts};
    use std::path::Path;
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::OwnedObjectPath;

    const LOGIN1: &str = "org.freedesktop.login1";

    fn env(name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }

    pub fn gather() -> SessionFacts {
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        let display = env("DISPLAY");
        let wayland_display = env("WAYLAND_DISPLAY");
        let runtime = env("XDG_RUNTIME_DIR");
        let wayland_socket = match (&wayland_display, &runtime) {
            (Some(w), _) if w.starts_with('/') => Path::new(w).exists(),
            (Some(w), Some(r)) if !w.contains('/') => Path::new(r).join(w).exists(),
            _ => false,
        };
        let x11_local_socket = display
            .as_deref()
            .and_then(super::x11_display_number)
            .is_some_and(|n| Path::new(&format!("/tmp/.X11-unix/X{n}")).exists());
        let container = Path::new("/.dockerenv").exists()
            || Path::new("/run/.containerenv").exists()
            || Path::new("/run/systemd/container").exists()
            || env("container").is_some();
        let ssh = ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
            .iter()
            .any(|k| env(k).is_some());
        let session_bus = Connection::session().is_ok();
        let mut facts = SessionFacts {
            uid,
            container,
            ssh,
            display,
            wayland_display,
            wayland_socket,
            x11_local_socket,
            session_bus,
            ..SessionFacts::default()
        };
        let Ok(system) = Connection::system() else {
            return facts;
        };
        let Ok(manager) = Proxy::new(
            &system,
            LOGIN1,
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
        ) else {
            return facts;
        };
        let seats: Result<Vec<(String, OwnedObjectPath)>, _> = manager.call("ListSeats", &());
        let Ok(seats) = seats else { return facts };
        facts.logind = true;
        facts.seats_exist = !seats.is_empty();

        // XDG_SESSION_ID first (set by PAM for the session's processes), then
        // this PID's session, then the user's graphical Display session —
        // apps autostarted under systemd --user scopes belong to no session.
        let mut candidates: Vec<(String, Result<OwnedObjectPath, zbus::Error>)> = Vec::new();
        if let Some(id) = env("XDG_SESSION_ID") {
            candidates.push((
                "XDG_SESSION_ID".into(),
                manager.call("GetSession", &(id.as_str(),)),
            ));
        }
        candidates.push((
            "process".into(),
            manager.call("GetSessionByPID", &(std::process::id(),)),
        ));
        for (source, path) in candidates {
            if let Ok(path) = path {
                if let Some(session) = read_session(&system, &path) {
                    facts.session = Some(session);
                    facts.session_source = Some(source);
                    return facts;
                }
            }
        }
        let user: Result<OwnedObjectPath, _> = manager.call("GetUser", &(uid,));
        if let Ok(user) = user {
            if let Ok(proxy) = Proxy::new(
                &system,
                LOGIN1,
                user.as_str(),
                "org.freedesktop.login1.User",
            ) {
                let display: Result<(String, OwnedObjectPath), _> = proxy.get_property("Display");
                if let Ok((id, path)) = display {
                    if !id.is_empty() {
                        facts.session = read_session(&system, &path);
                        facts.session_source = Some("user display".into());
                    }
                }
            }
        }
        facts
    }

    fn read_session(system: &Connection, path: &OwnedObjectPath) -> Option<LogindSession> {
        let p = Proxy::new(
            system,
            LOGIN1,
            path.as_str(),
            "org.freedesktop.login1.Session",
        )
        .ok()?;
        let user: (u32, OwnedObjectPath) = p.get_property("User").ok()?;
        let seat: (String, OwnedObjectPath) = p.get_property("Seat").ok()?;
        Some(LogindSession {
            id: p.get_property("Id").ok()?,
            uid: user.0,
            remote: p.get_property("Remote").ok()?,
            active: p.get_property("Active").ok()?,
            kind: p.get_property("Type").ok()?,
            class: p.get_property("Class").ok()?,
            seat: seat.0,
        })
    }
}
