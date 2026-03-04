use std::collections::HashMap;
use std::io::BufReader;
use std::path::Path;

use chrono::{DateTime, NaiveDateTime, Utc};

/// Extracted EXIF metadata from an image file.
pub struct ExifData {
    /// Key/value pairs for the variant's `source_metadata`.
    pub source_metadata: HashMap<String, String>,
    /// Parsed DateTimeOriginal, if available.
    pub date_taken: Option<DateTime<Utc>>,
    /// EXIF orientation tag (1-8), if present.
    pub orientation: Option<u16>,
}

impl ExifData {
    fn empty() -> Self {
        Self {
            source_metadata: HashMap::new(),
            date_taken: None,
            orientation: None,
        }
    }
}

/// Extract a clean string from an EXIF field.
///
/// Some cameras (notably Fujifilm) store ASCII tags like LensModel as
/// multi-component values where only the first component is meaningful
/// and the rest are empty. `display_value()` renders all of them as
/// comma-separated quoted strings. This function returns just the first
/// non-empty ASCII component, falling back to `display_value()` for
/// non-ASCII types.
fn clean_field_value(field: &exif::Field) -> String {
    if let exif::Value::Ascii(ref components) = field.value {
        for component in components {
            let s = String::from_utf8_lossy(component);
            let s = s.trim().trim_matches('\0');
            if !s.is_empty() {
                return s.to_string();
            }
        }
        return String::new();
    }
    field.display_value().to_string()
}

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

/// Parse GPS decimal degrees from a kamadak-exif Rational value.
fn parse_gps_decimal(field: &exif::Field, ref_val: &str) -> Option<f64> {
    if let exif::Value::Rational(ref rats) = field.value {
        if rats.len() >= 3 {
            let deg = rats[0].to_f64();
            let min = rats[1].to_f64();
            let sec = rats[2].to_f64();
            let decimal = deg + min / 60.0 + sec / 3600.0;
            return match ref_val.trim() {
                "S" | "W" => Some(-decimal),
                _ => Some(decimal),
            };
        }
    }
    None
}

/// Read EXIF orientation from in-memory image bytes (e.g. embedded JPEG from dcraw).
/// Returns None if EXIF data is missing or has no orientation tag.
pub fn orientation_from_bytes(data: &[u8]) -> Option<u16> {
    let mut cursor = std::io::Cursor::new(data);
    let exif = exif::Reader::new().read_from_container(&mut cursor).ok()?;
    exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| match f.value {
            exif::Value::Short(ref v) => v.first().copied(),
            _ => None,
        })
}

/// Extract EXIF metadata from a file. Infallible — returns empty data on any error.
pub fn extract(path: &Path) -> ExifData {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return ExifData::empty(),
    };
    let exif = match exif::Reader::new().read_from_container(&mut BufReader::new(file)) {
        Ok(e) => e,
        Err(_) => return ExifData::empty(),
    };

    let mut meta = HashMap::new();

    // Simple tag mappings
    let tag_map: &[(exif::Tag, &str)] = &[
        (exif::Tag::Make, "camera_make"),
        (exif::Tag::Model, "camera_model"),
        (exif::Tag::LensModel, "lens_model"),
        (exif::Tag::PhotographicSensitivity, "iso"),
        (exif::Tag::ExposureTime, "exposure_time"),
        (exif::Tag::FNumber, "f_number"),
        (exif::Tag::FocalLength, "focal_length"),
        (exif::Tag::PixelXDimension, "image_width"),
        (exif::Tag::PixelYDimension, "image_height"),
    ];

    for (tag, key) in tag_map {
        if let Some(field) = exif.get_field(*tag, exif::In::PRIMARY) {
            let val = clean_field_value(field);
            if !val.is_empty() {
                meta.insert(key.to_string(), val);
            }
        }
    }

    // Orientation
    let orientation = exif
        .get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| match f.value {
            exif::Value::Short(ref v) => v.first().copied(),
            _ => None,
        });
    if let Some(o) = orientation {
        meta.insert("orientation".to_string(), o.to_string());
    }

    // GPS latitude
    if let Some(lat) = exif.get_field(exif::Tag::GPSLatitude, exif::In::PRIMARY) {
        let ref_val = exif
            .get_field(exif::Tag::GPSLatitudeRef, exif::In::PRIMARY)
            .map(|f| f.display_value().to_string())
            .unwrap_or_default();
        let coord = lat.display_value().to_string();
        if !coord.is_empty() {
            meta.insert("gps_latitude".to_string(), format!("{coord} {ref_val}").trim().to_string());
        }
        if let Some(decimal) = parse_gps_decimal(lat, &ref_val) {
            meta.insert("gps_latitude_decimal".to_string(), decimal.to_string());
        }
    }

    // GPS longitude
    if let Some(lon) = exif.get_field(exif::Tag::GPSLongitude, exif::In::PRIMARY) {
        let ref_val = exif
            .get_field(exif::Tag::GPSLongitudeRef, exif::In::PRIMARY)
            .map(|f| f.display_value().to_string())
            .unwrap_or_default();
        let coord = lon.display_value().to_string();
        if !coord.is_empty() {
            meta.insert("gps_longitude".to_string(), format!("{coord} {ref_val}").trim().to_string());
        }
        if let Some(decimal) = parse_gps_decimal(lon, &ref_val) {
            meta.insert("gps_longitude_decimal".to_string(), decimal.to_string());
        }
    }

    // GPS altitude
    if let Some(alt) = exif.get_field(exif::Tag::GPSAltitude, exif::In::PRIMARY) {
        let val = alt.display_value().to_string();
        if !val.is_empty() {
            meta.insert("gps_altitude".to_string(), val);
        }
    }

    // Parse DateTimeOriginal
    let date_taken = exif
        .get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY)
        .and_then(|f| {
            let s = f.display_value().to_string();
            // Format: "YYYY:MM:DD HH:MM:SS" (sometimes quoted by display_value)
            let s = s.trim_matches('"');
            NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
                .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y:%m:%d %H:%M:%S"))
                .ok()
        })
        .map(|ndt| ndt.and_utc());

    // Store date_taken in source_metadata so it's available for fix-dates repair
    if let Some(ref dt) = date_taken {
        meta.insert("date_taken".to_string(), dt.to_rfc3339());
    }

    ExifData {
        source_metadata: meta,
        date_taken,
        orientation,
    }
}

/// Apply EXIF orientation transform to an image.
///
/// EXIF orientation values 1-8 map to combinations of rotation and flip:
/// 1 = no transform, 2 = flip horizontal, 3 = rotate 180°,
/// 4 = flip vertical, 5 = rotate 90° CW + flip horizontal,
/// 6 = rotate 90° CW, 7 = rotate 270° CW + flip horizontal,
/// 8 = rotate 270° CW.
pub fn apply_exif_orientation(
    img: image::DynamicImage,
    orientation: u16,
) -> image::DynamicImage {
    match orientation {
        1 => img,
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img, // unknown orientation, leave as-is
    }
}

/// Apply a manual rotation (in degrees clockwise) to an image.
///
/// Supported values: 0 (no-op), 90, 180, 270. Other values are ignored.
pub fn apply_rotation(img: image::DynamicImage, degrees: u16) -> image::DynamicImage {
    match degrees {
        0 => img,
        90 => img.rotate90(),
        180 => img.rotate180(),
        270 => img.rotate270(),
        _ => img,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn non_image_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "hello world").unwrap();

        let data = extract(&path);
        assert!(data.source_metadata.is_empty());
        assert!(data.date_taken.is_none());
    }

    #[test]
    fn nonexistent_file_returns_empty() {
        let data = extract(&PathBuf::from("/nonexistent/file.jpg"));
        assert!(data.source_metadata.is_empty());
        assert!(data.date_taken.is_none());
    }

    #[test]
    fn parse_dms_north() {
        let result = parse_dms_string("51 deg 30 min 26.36 sec N");
        assert!(result.is_some());
        let val = result.unwrap();
        assert!((val - 51.507322).abs() < 0.0001);
    }

    #[test]
    fn parse_dms_south() {
        let result = parse_dms_string("33 deg 51 min 54.00 sec S");
        assert!(result.is_some());
        let val = result.unwrap();
        assert!((val - (-33.865)).abs() < 0.001);
    }

    #[test]
    fn parse_dms_east() {
        let result = parse_dms_string("13 deg 23 min 0.00 sec E");
        assert!(result.is_some());
        let val = result.unwrap();
        assert!((val - 13.3833).abs() < 0.001);
    }

    #[test]
    fn parse_dms_west() {
        let result = parse_dms_string("0 deg 7 min 39.93 sec W");
        assert!(result.is_some());
        let val = result.unwrap();
        assert!(val < 0.0);
        assert!((val - (-0.12776)).abs() < 0.001);
    }

    #[test]
    fn parse_dms_zero() {
        let result = parse_dms_string("0 deg 0 min 0.00 sec N");
        assert!(result.is_some());
        assert!((result.unwrap()).abs() < 0.0001);
    }

    #[test]
    fn parse_dms_malformed() {
        assert!(parse_dms_string("").is_none());
        assert!(parse_dms_string("not a coordinate").is_none());
        assert!(parse_dms_string("abc deg def min ghi sec N").is_none());
    }

    #[test]
    fn fuji_lens_model_is_clean_string() {
        // Fuji cameras store LensModel as multi-component ASCII where only
        // the first component is the actual lens name.
        let path = PathBuf::from("/private/tmp/dam-test/fuji1.jpg");
        if !path.exists() {
            eprintln!("Skipping fuji test — sample file not found");
            return;
        }
        let data = extract(&path);
        let lens = data.source_metadata.get("lens_model").expect("lens_model should be present");
        assert!(
            !lens.contains(','),
            "lens_model should be a single value, got: {lens}"
        );
        assert_eq!(lens, "XF56mmF1.2 R");
    }

    #[test]
    fn exif_orientation_noop() {
        let img = image::DynamicImage::new_rgb8(100, 50);
        let result = apply_exif_orientation(img, 1);
        assert_eq!(result.width(), 100);
        assert_eq!(result.height(), 50);
    }

    #[test]
    fn exif_orientation_rotate90_swaps_dimensions() {
        let img = image::DynamicImage::new_rgb8(100, 50);
        let result = apply_exif_orientation(img, 6); // rotate 90° CW
        assert_eq!(result.width(), 50);
        assert_eq!(result.height(), 100);
    }

    #[test]
    fn exif_orientation_rotate270_swaps_dimensions() {
        let img = image::DynamicImage::new_rgb8(100, 50);
        let result = apply_exif_orientation(img, 8); // rotate 270° CW
        assert_eq!(result.width(), 50);
        assert_eq!(result.height(), 100);
    }

    #[test]
    fn exif_orientation_rotate180_preserves_dimensions() {
        let img = image::DynamicImage::new_rgb8(100, 50);
        let result = apply_exif_orientation(img, 3);
        assert_eq!(result.width(), 100);
        assert_eq!(result.height(), 50);
    }

    #[test]
    fn exif_orientation_flip_preserves_dimensions() {
        let img = image::DynamicImage::new_rgb8(100, 50);
        let result = apply_exif_orientation(img, 2); // flip horizontal
        assert_eq!(result.width(), 100);
        assert_eq!(result.height(), 50);
    }

    #[test]
    fn apply_rotation_90() {
        let img = image::DynamicImage::new_rgb8(100, 50);
        let result = apply_rotation(img, 90);
        assert_eq!(result.width(), 50);
        assert_eq!(result.height(), 100);
    }

    #[test]
    fn apply_rotation_180() {
        let img = image::DynamicImage::new_rgb8(100, 50);
        let result = apply_rotation(img, 180);
        assert_eq!(result.width(), 100);
        assert_eq!(result.height(), 50);
    }

    #[test]
    fn apply_rotation_270() {
        let img = image::DynamicImage::new_rgb8(100, 50);
        let result = apply_rotation(img, 270);
        assert_eq!(result.width(), 50);
        assert_eq!(result.height(), 100);
    }

    #[test]
    fn apply_rotation_0_noop() {
        let img = image::DynamicImage::new_rgb8(100, 50);
        let result = apply_rotation(img, 0);
        assert_eq!(result.width(), 100);
        assert_eq!(result.height(), 50);
    }

    #[test]
    fn apply_rotation_unknown_noop() {
        let img = image::DynamicImage::new_rgb8(100, 50);
        let result = apply_rotation(img, 45);
        assert_eq!(result.width(), 100);
        assert_eq!(result.height(), 50);
    }
}
