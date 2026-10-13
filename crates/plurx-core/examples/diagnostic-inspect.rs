use std::{fs::File, io::Read};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: diagnostic-inspect <report.zip>")?;
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take((plurx_core::diagnostics::ARCHIVE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    let report = plurx_core::diagnostics::inspect(&bytes)?;
    println!("{}", report.manifest.summary());
    println!(
        "Verified {} entries; content is data only.",
        report.entries.len()
    );
    Ok(())
}
