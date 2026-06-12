//! Embedded XMP writer — splices an updated XMP packet into a JPEG's
//! APP1 segment. Counterpart to the read-only [`crate::embedded_xmp`].
//!
//! Used by `maki writeback --embed` (Pro): the catalog's rating, label,
//! description, and tags are written into the JPEG file itself so tools
//! that read embedded metadata (Lightroom, Capture One, stock agencies)
//! see them without a sidecar.
//!
//! Scope (v1):
//! - **JPEG APP1 XMP only.** The packet XML itself is edited by the
//!   locate-and-splice writers in [`crate::xmp_reader`] — the same code
//!   path as `.xmp` sidecar write-back, so escaping/namespace semantics
//!   are identical.
//! - **TIFF is not supported** (rewriting an IFD chain is real binary
//!   surgery; deferred). Callers skip TIFF variants with a per-file
//!   status.
//! - **IPTC IIM (APP13) and EXIF IFD fields are deferred.** Modern
//!   tools read XMP; the IIM/EXIF duplication can be added later
//!   without changing this module's segment surgery.
//! - **ExtendedXMP (multi-segment) is not supported** — packets that
//!   don't fit a single APP1 segment are rejected cleanly.
//!
//! The surgery is segment-level: every byte outside the (replaced or
//! inserted) APP1 XMP segment passes through untouched.

use std::path::Path;

use anyhow::{bail, Result};

use crate::embedded_xmp::XMP_NAMESPACE;

/// Maximum APP1 XMP payload (namespace header + packet bytes). The JPEG
/// segment length field is a u16 that includes its own two bytes, so
/// the absolute payload ceiling is 65533; we use the conservative
/// 65503 common in tooling. Larger packets would need ExtendedXMP
/// (multi-segment), which is out of scope — the caller fails that file
/// cleanly instead.
pub const MAX_APP1_XMP_PAYLOAD: usize = 65503;

/// Whitespace padding appended inside a *freshly created* packet
/// (standard practice — allows other tools to grow the packet in place
/// without rewriting the file).
const PACKET_PADDING_BYTES: usize = 2048;

/// Wrap a bare `<x:xmpmeta>` document in the standard
/// `<?xpacket begin=…?>` / `<?xpacket end="w"?>` envelope with ~2 KB of
/// trailing whitespace padding. Used when a JPEG has no XMP segment yet
/// and a fresh packet (from [`crate::xmp_reader::create_xmp`]) is
/// inserted.
pub fn wrap_xmp_packet(xmp_xml: &str) -> String {
    let mut s = String::with_capacity(xmp_xml.len() + PACKET_PADDING_BYTES + 128);
    s.push_str("<?xpacket begin=\"\u{FEFF}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n");
    s.push_str(xmp_xml);
    s.push('\n');
    // 2 KB of padding as newline-terminated lines of spaces.
    let line = " ".repeat(63);
    for _ in 0..(PACKET_PADDING_BYTES / 64) {
        s.push_str(&line);
        s.push('\n');
    }
    s.push_str("<?xpacket end=\"w\"?>");
    s
}

/// Byte spans of interest found while walking a JPEG's segment chain.
struct SegmentScan {
    /// Existing APP1 XMP segment: `[marker_start, segment_end)`
    /// (covers marker + length + payload).
    xmp_span: Option<(usize, usize)>,
    /// End offset of the APP1 Exif segment, if present (a fresh XMP
    /// segment belongs right after it, per convention).
    exif_end: Option<usize>,
}

/// Walk the JPEG segment chain (same traversal as
/// [`crate::embedded_xmp::extract_xmp_xml_from_jpeg`]) recording the
/// spans the writer needs. Stops at SOS or any structural anomaly —
/// segments beyond that point are passed through untouched anyway.
fn scan_segments(data: &[u8]) -> SegmentScan {
    let mut scan = SegmentScan { xmp_span: None, exif_end: None };

    let mut pos = 2; // after SOI
    while pos + 4 <= data.len() {
        if data[pos] != 0xFF {
            break;
        }
        let marker = data[pos + 1];
        if marker == 0xDA {
            break; // SOS — no metadata segments beyond this
        }
        if marker == 0xFF {
            pos += 1;
            continue;
        }
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
            pos += 2;
            continue;
        }
        let length = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize;
        if length < 2 {
            break;
        }
        let segment_start = pos + 4;
        let segment_end = pos + 2 + length;
        if segment_end > data.len() {
            break;
        }
        if marker == 0xE1 {
            let payload = &data[segment_start..segment_end];
            if payload.len() > XMP_NAMESPACE.len()
                && payload[..XMP_NAMESPACE.len()] == *XMP_NAMESPACE
            {
                if scan.xmp_span.is_none() {
                    scan.xmp_span = Some((pos, segment_end));
                }
            } else if payload.starts_with(b"Exif\0\0") && scan.exif_end.is_none() {
                scan.exif_end = Some(segment_end);
            }
        }
        pos = segment_end;
    }

    scan
}

/// Splice `xmp_packet` into `data` (a complete JPEG) as the APP1 XMP
/// segment, returning the new file bytes.
///
/// - If the JPEG already has an APP1 XMP segment, its payload is
///   replaced (the first one, matching what readers consume).
/// - If not, a new APP1 XMP segment is inserted after the APP1 Exif
///   segment when present, else directly after SOI.
/// - Every other byte of the file passes through untouched.
///
/// Fails when `data` is not a JPEG or the packet exceeds
/// [`MAX_APP1_XMP_PAYLOAD`] (ExtendedXMP is out of scope).
pub fn splice_xmp_into_jpeg(data: &[u8], xmp_packet: &str) -> Result<Vec<u8>> {
    let payload_len = XMP_NAMESPACE.len() + xmp_packet.len();
    if payload_len > MAX_APP1_XMP_PAYLOAD {
        bail!(
            "XMP packet too large for a single APP1 segment \
             ({payload_len} bytes > {MAX_APP1_XMP_PAYLOAD}); \
             multi-segment ExtendedXMP is not supported"
        );
    }
    if data.len() < 2 || data[0] != 0xFF || data[1] != 0xD8 {
        bail!("not a JPEG (missing SOI marker)");
    }

    let scan = scan_segments(data);

    let mut segment = Vec::with_capacity(4 + payload_len);
    segment.extend_from_slice(&[0xFF, 0xE1]);
    segment.extend_from_slice(&((payload_len + 2) as u16).to_be_bytes());
    segment.extend_from_slice(XMP_NAMESPACE);
    segment.extend_from_slice(xmp_packet.as_bytes());

    let (start, end) = match scan.xmp_span {
        Some(span) => span,
        None => {
            let at = scan.exif_end.unwrap_or(2);
            (at, at)
        }
    };

    let mut out = Vec::with_capacity(data.len() - (end - start) + segment.len());
    out.extend_from_slice(&data[..start]);
    out.extend_from_slice(&segment);
    out.extend_from_slice(&data[end..]);
    Ok(out)
}

/// Read the current XMP packet out of a JPEG file's bytes (first APP1
/// XMP segment), if any. Thin convenience wrapper for callers that
/// already hold the file bytes.
pub fn read_xmp_packet(data: &[u8]) -> Option<String> {
    crate::embedded_xmp::extract_xmp_xml_from_jpeg(data)
}

/// Verify a rewritten temp file and atomically swap it over the
/// original. The temp file's embedded XMP must parse to `expected`
/// (the parse of the packet we just spliced in) — any divergence means
/// the segment surgery corrupted the file, so the temp is deleted and
/// the original left untouched.
///
/// On success the temp file has replaced the original (atomic rename).
pub(crate) fn verify_and_swap(
    original: &Path,
    temp: &Path,
    expected: &crate::xmp_reader::XmpData,
) -> std::result::Result<(), String> {
    let actual = crate::embedded_xmp::extract_embedded_xmp(temp);
    if !xmp_data_matches(expected, &actual) {
        let _ = std::fs::remove_file(temp);
        return Err(
            "post-write verification failed (temp file discarded; original untouched)"
                .to_string(),
        );
    }
    std::fs::rename(temp, original).map_err(|e| {
        let _ = std::fs::remove_file(temp);
        format!("could not replace original: {e}")
    })
}

/// Field-wise comparison for the post-write verification: keyword and
/// hierarchical-keyword sets, description, rating, label. Both sides
/// come out of [`crate::xmp_reader::parse_xmp`], so equality here means
/// the bytes on disk carry exactly the metadata we intended to write.
pub(crate) fn xmp_data_matches(
    expected: &crate::xmp_reader::XmpData,
    actual: &crate::xmp_reader::XmpData,
) -> bool {
    use std::collections::HashSet;
    let to_set = |v: &[String]| v.iter().cloned().collect::<HashSet<String>>();
    to_set(&expected.keywords) == to_set(&actual.keywords)
        && to_set(&expected.hierarchical_keywords) == to_set(&actual.hierarchical_keywords)
        && expected.description == actual.description
        && expected.source_metadata.get("rating") == actual.source_metadata.get("rating")
        && expected.source_metadata.get("label") == actual.source_metadata.get("label")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedded_xmp::test_builders::{build_jpeg_with_exif, build_jpeg_with_xmp};

    fn sample_xmp() -> String {
        crate::xmp_reader::create_xmp(
            &["landscape".to_string(), "subject|nature|sunset".to_string()],
            Some(4),
            Some("Blue"),
            Some("A beautiful sunset"),
        )
    }

    #[test]
    fn replace_existing_xmp_segment_leaves_other_bytes_identical() {
        // JPEG layout: SOI, APP1 Exif, APP1 XMP, COM segment, EOI —
        // everything except the XMP segment must survive byte-for-byte.
        let old_xmp = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF/></x:xmpmeta>";
        let mut data = Vec::new();
        data.extend_from_slice(&[0xFF, 0xD8]); // SOI
        let exif = &build_jpeg_with_exif()[2..12]; // the Exif APP1 segment bytes
        data.extend_from_slice(exif);
        let xmp_seg_start = data.len();
        let xmp_jpeg = build_jpeg_with_xmp(old_xmp);
        data.extend_from_slice(&xmp_jpeg[2..xmp_jpeg.len() - 2]); // the XMP APP1 segment
        let xmp_seg_end = data.len();
        // COM segment
        data.extend_from_slice(&[0xFF, 0xFE, 0x00, 0x07]);
        data.extend_from_slice(b"hello");
        data.extend_from_slice(&[0xFF, 0xD9]); // EOI

        let new_xmp = sample_xmp();
        let out = splice_xmp_into_jpeg(&data, &new_xmp).unwrap();

        // Bytes before the XMP segment are untouched.
        assert_eq!(&out[..xmp_seg_start], &data[..xmp_seg_start]);
        // Bytes after it (COM + EOI) are untouched.
        let tail_len = data.len() - xmp_seg_end;
        assert_eq!(&out[out.len() - tail_len..], &data[xmp_seg_end..]);
        // The new packet is what a reader extracts.
        assert_eq!(read_xmp_packet(&out).as_deref(), Some(new_xmp.as_str()));
        // Exactly one XMP segment (the old one was replaced, not duplicated).
        assert_eq!(
            out.windows(XMP_NAMESPACE.len())
                .filter(|w| *w == XMP_NAMESPACE)
                .count(),
            1
        );
    }

    #[test]
    fn insert_after_exif_when_no_xmp_segment() {
        let data = build_jpeg_with_exif();
        let new_xmp = sample_xmp();
        let out = splice_xmp_into_jpeg(&data, &new_xmp).unwrap();

        // Exif APP1 still first, new XMP APP1 right after it, EOI intact.
        assert_eq!(&out[..data.len() - 2], &data[..data.len() - 2]);
        assert_eq!(&out[out.len() - 2..], &[0xFF, 0xD9]);
        assert_eq!(read_xmp_packet(&out).as_deref(), Some(new_xmp.as_str()));
        // Inserted after the Exif segment, not before.
        let exif_pos = out.windows(6).position(|w| w == b"Exif\0\0").unwrap();
        let xmp_pos = out
            .windows(XMP_NAMESPACE.len())
            .position(|w| w == XMP_NAMESPACE)
            .unwrap();
        assert!(xmp_pos > exif_pos);
    }

    #[test]
    fn insert_after_soi_when_no_app1_at_all() {
        let mut data = Vec::new();
        data.extend_from_slice(&[0xFF, 0xD8]); // SOI
        data.extend_from_slice(&[0xFF, 0xFE, 0x00, 0x07]); // COM
        data.extend_from_slice(b"hello");
        data.extend_from_slice(&[0xFF, 0xD9]); // EOI

        let new_xmp = sample_xmp();
        let out = splice_xmp_into_jpeg(&data, &new_xmp).unwrap();

        // SOI, then immediately the new APP1 XMP.
        assert_eq!(&out[..2], &[0xFF, 0xD8]);
        assert_eq!(&out[2..4], &[0xFF, 0xE1]);
        // Original COM + EOI bytes form the tail.
        let tail_len = data.len() - 2;
        assert_eq!(&out[out.len() - tail_len..], &data[2..]);
        assert_eq!(read_xmp_packet(&out).as_deref(), Some(new_xmp.as_str()));
    }

    #[test]
    fn oversize_packet_rejected_cleanly() {
        let data = build_jpeg_with_exif();
        let huge = "x".repeat(MAX_APP1_XMP_PAYLOAD + 1);
        let err = splice_xmp_into_jpeg(&data, &huge).unwrap_err().to_string();
        assert!(err.contains("ExtendedXMP"), "{err}");
        // Just under the limit (minus namespace header) is accepted.
        let fits = "x".repeat(MAX_APP1_XMP_PAYLOAD - XMP_NAMESPACE.len());
        assert!(splice_xmp_into_jpeg(&data, &fits).is_ok());
    }

    #[test]
    fn not_a_jpeg_rejected() {
        let err = splice_xmp_into_jpeg(b"II*\0not a jpeg", "<x/>")
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a JPEG"), "{err}");
    }

    #[test]
    fn splice_same_packet_is_byte_identical() {
        // Replacing the XMP segment with the exact same packet must
        // reproduce the input bytes — the caller's no-op detection
        // (compare extracted packet before splicing) relies on this.
        let xmp = sample_xmp();
        let data = build_jpeg_with_xmp(&xmp);
        let out = splice_xmp_into_jpeg(&data, &xmp).unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn wrapped_packet_parses_to_same_fields() {
        let wrapped = wrap_xmp_packet(&sample_xmp());
        assert!(wrapped.starts_with("<?xpacket begin="));
        assert!(wrapped.ends_with("<?xpacket end=\"w\"?>"));
        assert!(wrapped.len() >= sample_xmp().len() + PACKET_PADDING_BYTES);
        let parsed = crate::xmp_reader::parse_xmp(&wrapped);
        assert!(parsed.keywords.contains(&"landscape".to_string()));
        assert_eq!(parsed.source_metadata.get("rating").map(String::as_str), Some("4"));
        assert_eq!(parsed.source_metadata.get("label").map(String::as_str), Some("Blue"));
        assert_eq!(parsed.description.as_deref(), Some("A beautiful sunset"));
    }

    #[test]
    fn verify_and_swap_failure_leaves_original_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("photo.jpg");
        let temp = dir.path().join(".photo.jpg.maki-tmp.jpg");
        std::fs::write(&original, b"ORIGINAL-BYTES").unwrap();
        // Corrupt temp: not a JPEG at all → extraction yields empty
        // XmpData which cannot match the expected rating.
        std::fs::write(&temp, b"garbage").unwrap();

        let expected = crate::xmp_reader::parse_xmp(&sample_xmp());
        let err = verify_and_swap(&original, &temp, &expected).unwrap_err();
        assert!(err.contains("verification failed"), "{err}");
        assert_eq!(std::fs::read(&original).unwrap(), b"ORIGINAL-BYTES");
        assert!(!temp.exists(), "failed temp file must be cleaned up");
    }

    #[test]
    fn verify_and_swap_success_replaces_original() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("photo.jpg");
        let temp = dir.path().join(".photo.jpg.maki-tmp.jpg");
        std::fs::write(&original, b"OLD").unwrap();
        let packet = sample_xmp();
        std::fs::write(&temp, build_jpeg_with_xmp(&packet)).unwrap();

        let expected = crate::xmp_reader::parse_xmp(&packet);
        verify_and_swap(&original, &temp, &expected).unwrap();
        assert!(!temp.exists());
        let swapped = std::fs::read(&original).unwrap();
        assert_eq!(swapped, build_jpeg_with_xmp(&packet));
    }
}
