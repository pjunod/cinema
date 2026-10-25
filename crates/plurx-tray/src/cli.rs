//! Command line. Paths that reach the controller or a receipt must be
//! absolute; nothing is read from the environment to choose an executable.
use crate::receipt::SmokeAction;
use crate::runner::DEFAULT_CONTROLLER;
use std::path::PathBuf;
use std::time::Duration;

pub const USAGE: &str = "\
usage: plurx-tray [--window | --autostart] [--control ABS] [--installation ABS]
       plurx-tray --smoke-receipt ABS.json [--smoke-action quit|stop|restart]
                  [--smoke-timeout SECONDS] [--control ABS] [--installation ABS] [--window]

  --window        open the control window (the application-menu launcher; default)
  --autostart     login start: only for an eligible local graphical session whose
                  autostart preference is on, and only once a tray host exists
                  (waits at most 30 seconds, then exits quietly)
  --control       the plurx-control executable (default /usr/bin/plurx-control)
  --installation  forwarded to plurx-control --installation (unprivileged testing)
  --smoke-receipt run the real tray and window once, write a plurx-tray-smoke-v1
                  receipt and quit; never changes startup or update policy";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Smoke {
    pub receipt: PathBuf,
    pub action: SmokeAction,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Window,
    Autostart,
    Smoke(Smoke),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub mode: Mode,
    pub control: PathBuf,
    pub installation: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Run(Options),
    Help,
    Version,
}

fn absolute(flag: &str, value: Option<String>) -> Result<PathBuf, String> {
    let value = value.ok_or_else(|| format!("{flag} needs a value"))?;
    if !value.starts_with('/') || value.contains('\0') {
        return Err(format!("{flag} must be an absolute path"));
    }
    Ok(PathBuf::from(value))
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Parsed, String> {
    let mut args = args.into_iter();
    let mut window = false;
    let mut autostart = false;
    let mut control = PathBuf::from(DEFAULT_CONTROLLER);
    let mut installation = None;
    let mut receipt = None;
    let mut action = None;
    let mut timeout = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--version" => return Ok(Parsed::Version),
            "--window" => window = true,
            "--autostart" => autostart = true,
            "--control" => control = absolute("--control", args.next())?,
            "--installation" => installation = Some(absolute("--installation", args.next())?),
            "--smoke-receipt" => receipt = Some(absolute("--smoke-receipt", args.next())?),
            "--smoke-action" => {
                let value = args.next().ok_or("--smoke-action needs a value")?;
                action = Some(SmokeAction::parse(&value).ok_or_else(|| {
                    format!("--smoke-action must be quit, stop or restart (got {value})")
                })?);
            }
            "--smoke-timeout" => {
                let value = args.next().ok_or("--smoke-timeout needs a value")?;
                let seconds: u64 = value
                    .parse()
                    .map_err(|_| format!("--smoke-timeout must be seconds (got {value})"))?;
                if !(1..=600).contains(&seconds) {
                    return Err("--smoke-timeout must be 1..600".into());
                }
                timeout = Some(Duration::from_secs(seconds));
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if autostart && (window || receipt.is_some()) {
        return Err("--autostart cannot be combined with --window or --smoke-receipt".into());
    }
    let mode = match receipt {
        Some(receipt) => Mode::Smoke(Smoke {
            receipt,
            action: action.unwrap_or(SmokeAction::Quit),
            timeout: timeout.unwrap_or(Duration::from_secs(60)),
        }),
        None if action.is_some() || timeout.is_some() => {
            return Err("--smoke-action and --smoke-timeout need --smoke-receipt".into())
        }
        None if autostart => Mode::Autostart,
        None => Mode::Window,
    };
    Ok(Parsed::Run(Options {
        mode,
        control,
        installation,
    }))
}
