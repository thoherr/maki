//! File-format vocabulary: extension groups and the pure classification
//! helpers derived from them (`determine_asset_type`, `is_raw_extension`,
//! `strip_media_extension`, …) plus `normalize_rating`. No I/O, no
//! storage — the tables every layer agrees on.

use super::AssetType;

/// File type group definitions: (name, extensions, default_on).
pub(crate) const GROUPS: &[(&str, &[&str], bool)] = &[
    (
        "images",
        &[
            "jpg", "jpeg", "png", "gif", "bmp", "tiff", "tif", "webp", "heic", "heif", "svg",
            "ico", "psd", "xcf", // standard image formats
            "raw", "cr2", "cr3", "crw", "nef", "nrw", "arw", "sr2", "srf", "orf", "rw2",
            "dng", "raf", "pef", "srw", "mrw", "3fr", "fff", "iiq", "erf", "kdc", "dcr",
            "mef", "mos", "rwl", "bay", "x3f", // RAW
        ],
        true,
    ),
    (
        "video",
        &[
            "mp4", "mov", "avi", "mkv", "wmv", "flv", "webm", "m4v", "mpg", "mpeg", "3gp",
            "mts", "m2ts",
        ],
        true,
    ),
    (
        "audio",
        &[
            "mp3", "wav", "flac", "aac", "ogg", "wma", "m4a", "aiff", "alac",
        ],
        true,
    ),
    ("xmp", &["xmp"], true),
    (
        "documents",
        &[
            "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "md", "rtf", "csv",
            "json", "xml", "html", "htm",
        ],
        false,
    ),
    ("captureone", &["cos", "cot", "cop"], false),
    ("rawtherapee", &["pp3"], false),
    ("dxo", &["dop"], false),
    ("on1", &["on1"], false),
];

/// Recipe group names — extensions in these groups are treated as recipe sidecars.
pub(crate) const RECIPE_GROUPS: &[&str] = &["xmp", "captureone", "rawtherapee", "dxo", "on1"];

/// Normalize a rating value: convert MicrosoftPhoto:Rating percentage scale (1-100)
/// to xmp:Rating 1-5 scale. Values already in 1-5 range pass through unchanged.
pub fn normalize_rating(r: u8) -> u8 {
    if r <= 5 {
        r
    } else {
        // MicrosoftPhoto:Rating percentages: 1→1, 25→2, 50→3, 75→4, 99/100→5
        match r {
            1..=12 => 1,
            13..=37 => 2,
            38..=62 => 3,
            63..=87 => 4,
            _ => 5,
        }
    }
}

/// Check if a file extension supports embedded XMP extraction (JPEG/TIFF).
pub fn is_embedded_xmp_extension(ext: &str) -> bool {
    matches!(ext.to_lowercase().as_str(), "jpg" | "jpeg" | "tif" | "tiff")
}

/// Determine the asset type from a file extension.
pub fn determine_asset_type(ext: &str) -> AssetType {
    match ext.to_lowercase().as_str() {
        // Images
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "tiff" | "tif" | "webp" | "heic" | "heif"
        | "raw" | "cr2" | "cr3" | "crw" | "nef" | "nrw" | "arw" | "sr2" | "srf" | "orf"
        | "rw2" | "dng" | "raf" | "pef" | "srw" | "mrw" | "3fr" | "fff" | "iiq" | "erf"
        | "kdc" | "dcr" | "mef" | "mos" | "rwl" | "bay" | "x3f"
        | "svg" | "ico" | "psd" | "xcf" => AssetType::Image,
        // Video
        "mp4" | "mov" | "avi" | "mkv" | "wmv" | "flv" | "webm" | "m4v" | "mpg" | "mpeg"
        | "3gp" | "mts" | "m2ts" => AssetType::Video,
        // Audio
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "wma" | "m4a" | "aiff" | "alac" => {
            AssetType::Audio
        }
        // Documents
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "md" | "rtf"
        | "csv" | "json" | "xml" | "html" | "htm" => AssetType::Document,
        _ => AssetType::Other,
    }
}

/// Check if a file extension is a RAW camera format.
pub fn is_raw_extension(ext: &str) -> bool {
    matches!(
        ext.to_lowercase().as_str(),
        "raw" | "cr2" | "cr3" | "crw" | "nef" | "nrw" | "arw" | "sr2" | "srf"
            | "orf" | "rw2" | "dng" | "raf" | "pef" | "srw" | "mrw"
            | "3fr" | "fff" | "iiq" | "erf" | "kdc" | "dcr"
            | "mef" | "mos" | "rwl" | "bay" | "x3f"
    )
}

/// Infer the processing software from a recipe file extension.
pub fn determine_recipe_software(ext: &str) -> &'static str {
    match ext.to_lowercase().as_str() {
        "xmp" => "Adobe/CaptureOne",
        "cos" | "cot" | "cop" => "CaptureOne",
        "pp3" => "RawTherapee",
        "dop" => "DxO",
        "on1" => "ON1",
        _ => "Unknown",
    }
}

/// True if `ext` is a media (non-recipe) extension of any file-type group,
/// regardless of which groups an import filter has enabled.
pub fn is_media_extension(ext: &str) -> bool {
    let lower = ext.to_lowercase();
    GROUPS
        .iter()
        .filter(|(name, _, _)| !RECIPE_GROUPS.contains(name))
        .any(|(_, extensions, _)| extensions.contains(&lower.as_str()))
}

/// Strip a trailing media extension from a recipe file's stem so that
/// `DSC_001.NEF.xmp` (darktable / `[writeback] sidecar_naming = "full"`)
/// resolves to the same parent as `DSC_001.xmp` (Capture One, Lightroom).
/// Stems without such a suffix are returned unchanged.
pub fn strip_media_extension(stem: &str) -> &str {
    match stem.rsplit_once('.') {
        Some((base, inner)) if !base.is_empty() && is_media_extension(inner) => base,
        _ => stem,
    }
}
