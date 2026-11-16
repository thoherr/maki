//! Pure geographic helpers shared by the EXIF reader and `Variant`
//! (GPS fallback from DMS display strings).

/// Parse a DMS string like `"51 deg 30 min 26.36 sec N"` to decimal degrees.
///
/// Used by the GPS backfill migration to convert existing display strings stored
/// in variant `source_metadata` to decimal coordinates.
pub fn parse_dms_string(dms: &str) -> Option<f64> {
    // Expected format: "D deg M min S sec [N/S/E/W]"
    let parts: Vec<&str> = dms.split_whitespace().collect();
    if parts.len() < 6 {
        return None;
    }
    let deg: f64 = parts[0].parse().ok()?;
    // parts[1] should be "deg"
    let min: f64 = parts[2].parse().ok()?;
    // parts[3] should be "min"
    let sec: f64 = parts[4].parse().ok()?;
    // parts[5] should be "sec" — sometimes ref is parts[6], sometimes parts[5] is the ref
    let ref_val = *parts.last()?;
    let decimal = deg + min / 60.0 + sec / 3600.0;
    match ref_val {
        "S" | "W" => Some(-decimal),
        "N" | "E" => Some(decimal),
        _ => Some(decimal), // no ref → positive
    }
}
