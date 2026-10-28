// Supported public libdovi XML importer, not a hand-written XML/RPU mapping.
// Match dovi_tool's generator.rs config_from_xml/GenerateProfile override.
use dolby_vision::{
    rpu::{dovi_rpu::DoviRpu, generate::GenerateProfile},
    xml::{CmXmlParser, XmlParserOpts},
};
use serde_json::json;
use std::{fs, path::Path};

pub fn generate(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 6 {
        return Err("OUTPUT xml-reference XML WIDTH HEIGHT".into());
    }
    let width: u16 = args[4].parse()?;
    let height: u16 = args[5].parse()?;
    if width == 0 || width > 4096 || height == 0 || height > 2160 {
        return Err("bounded reference canvas required".into());
    }
    let xml = Path::new(&args[3]);
    if fs::metadata(xml)?.len() > 4 * 1024 * 1024 {
        return Err("bounded XML input required".into());
    }
    let parser = CmXmlParser::parse_file(
        xml,
        XmlParserOpts {
            canvas_width: Some(width),
            canvas_height: Some(height),
        },
    )?;
    let mut config = parser.config;
    config.profile = GenerateProfile::Profile81;
    // Public generate_rpu_list emits shots sequentially, not at shot.start.
    // Refuse gaps/overlaps rather than silently associating the wrong frame.
    let mut endpoint = 0;
    for shot in &config.shots {
        if shot.start != endpoint || shot.duration == 0 {
            return Err("XML shots must cover contiguous source-frame order".into());
        }
        endpoint = endpoint
            .checked_add(shot.duration)
            .ok_or("frame overflow")?;
        if endpoint > 16
            || shot
                .frame_edits
                .iter()
                .any(|e| e.edit_offset >= shot.duration)
        {
            return Err("bounded frame/edit coverage required".into());
        }
    }
    if endpoint == 0 || endpoint != config.length {
        return Err("complete nonempty XML frame coverage required".into());
    }
    let rpus = config.generate_rpu_list()?;
    if rpus.len() != endpoint {
        return Err("generated RPU frame count mismatch".into());
    }
    fs::create_dir(root)?; // Never reuse an earlier conversion attempt.
    fs::write(
        root.join("imported-config.json"),
        serde_json::to_vec_pretty(&config)?,
    )?;
    let mut frames = Vec::new();
    for (index, rpu) in rpus.iter().enumerate() {
        let nal = rpu.write_hevc_unspec62_nalu()?;
        let parsed = DoviRpu::parse_unspec62_nalu(&nal)?;
        let nal_name = format!("frame-{index:03}.nal");
        let parsed_name = format!("frame-{index:03}.json");
        fs::write(root.join(&nal_name), &nal)?;
        fs::write(root.join(&parsed_name), serde_json::to_vec_pretty(&parsed)?)?;
        let shot = config
            .shots
            .iter()
            .find(|s| index >= s.start && index < s.start + s.duration)
            .ok_or("generated frame has no XML source shot")?;
        frames.push(json!({"source_frame_index": index, "shot_id": shot.id,
            "shot_start": shot.start, "edit_offset": index - shot.start,
            "nal": nal_name, "parsed_rpu": parsed_name}));
    }
    fs::write(
        root.join("conversion.json"),
        serde_json::to_vec_pretty(&json!({
            "schema": "public-libdovi-xml-reference-v1",
            "library_revision": "83e1fdad6dcd5995556235946e7c5c0f9010d5a1",
            "method": "CmXmlParser::parse_file; Profile81; generate_rpu_list; serialize; parse",
            "xml": args[3], "width": width, "height": height, "frames": frames,
            "scope": "supported public XML import and actual RPU roundtrip; not Dolby algorithm equivalence"
        }))?,
    )?;
    Ok(())
}
