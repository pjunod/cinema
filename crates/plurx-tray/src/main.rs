//! plurx-tray: the Linux desktop companion (see the crate documentation).
use plurx_tray::cli::{self, Parsed};

fn main() {
    let code = match cli::parse(std::env::args().skip(1)) {
        Ok(Parsed::Help) => {
            println!("{}", cli::USAGE);
            0
        }
        Ok(Parsed::Version) => {
            println!(
                "plurx-tray {} ({})",
                plurx_tray::APP_VERSION,
                plurx_tray::app_build()
            );
            0
        }
        Ok(Parsed::Run(options)) => run(options),
        Err(error) => {
            eprintln!("plurx-tray: {error}\n{}", cli::USAGE);
            2
        }
    };
    std::process::exit(code);
}

#[cfg(target_os = "linux")]
fn run(options: cli::Options) -> i32 {
    plurx_tray::linux::run(options)
}

#[cfg(not(target_os = "linux"))]
fn run(_options: cli::Options) -> i32 {
    eprintln!("plurx-tray runs on Linux desktops; macOS uses Plurx Server.app");
    2
}
