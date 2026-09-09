use std::env;

fn main() {
    let profile = env::var("PROFILE").expect("Cargo must identify the active profile");
    let opt_level = env::var("OPT_LEVEL").expect("Cargo must identify the optimization level");
    let debug = env::var("DEBUG").expect("Cargo must identify the debug-info setting");
    println!(
        "cargo:rustc-env=PLURX_BUILD_PROFILE=profile={profile};opt-level={opt_level};debug={debug}"
    );
}
