use dolby_vision::rpu::dovi_rpu::DoviRpu;
use std::{fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(std::env::args().nth(1).ok_or("artifact directory required")?);
    let mut paths = fs::read_dir(&root)?.map(|entry| entry.map(|e| e.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    for path in paths {
        if !path.to_string_lossy().ends_with(".rpu.nal") {
            continue;
        }
        let data = fs::read(&path)?;
        let rpu = DoviRpu::parse_unspec62_nalu(&data)?;
        fs::write(path.with_extension("json"), serde_json::to_string_pretty(&rpu)?)?;
        // Public write_rpu returns decoded RBSP (no NAL header or EPB insertion).
        fs::write(path.with_extension("rbsp"), rpu.write_rpu()?)?;
    }
    Ok(())
}
