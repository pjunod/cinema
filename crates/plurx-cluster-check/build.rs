use std::env;

fn main() {
    println!("cargo:rerun-if-env-changed=PLURX_BUILD_SHA");
    if let Some(build_sha) = env::var("PLURX_BUILD_SHA")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
    {
        assert!(
            build_sha.len() == 40 && build_sha.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "PLURX_BUILD_SHA must be a full 40-character Git SHA"
        );
        println!("cargo:rustc-env=PLURX_BUILD_SHA={build_sha}");
    }
    let profile = env::var("PROFILE").expect("Cargo must identify the active profile");
    let opt_level = env::var("OPT_LEVEL").expect("Cargo must identify the optimization level");
    let debug = env::var("DEBUG").expect("Cargo must identify the debug-info setting");
    println!(
        "cargo:rustc-env=PLURX_BUILD_PROFILE=profile={profile};opt-level={opt_level};debug={debug}"
    );
}
