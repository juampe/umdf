//! Cheap metadata extraction: filesystem times, pixel dimensions, EXIF date.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::time::SystemTime;

use chrono::{DateTime, NaiveDateTime, Utc};

/// Read `(width, height)` from the image header without decoding pixels.
/// Returns `None` for formats the `image` crate cannot sniff.
pub fn image_dimensions(path: &Path) -> Option<(u32, u32)> {
    image::image_dimensions(path).ok()
}

/// EXIF facts used by the originality heuristic.
#[derive(Debug, Clone, Default)]
pub struct ExifMeta {
    /// `DateTimeOriginal`, falling back to `DateTime` (IFD0).
    pub taken: Option<DateTime<Utc>>,
    /// Number of fields in the EXIF block (richness proxy).
    pub field_count: u32,
    /// Make/Model present (the file came from a camera or phone camera).
    pub camera: bool,
}

/// Parse the EXIF block once and return the facts we need.
pub fn exif_meta(path: &Path) -> Option<ExifMeta> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let exif = exif::Reader::new()
        .read_from_container(&mut reader)
        .ok()?;

    let field_count = exif.fields().count() as u32;
    let camera = exif
        .get_field(exif::Tag::Make, exif::In::PRIMARY)
        .is_some()
        || exif
            .get_field(exif::Tag::Model, exif::In::PRIMARY)
            .is_some();

    let taken = exif
        .get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY)
        .or_else(|| exif.get_field(exif::Tag::DateTime, exif::In::PRIMARY))
        .and_then(|field| {
            NaiveDateTime::parse_from_str(field.display_value().to_string().trim(), "%Y-%m-%d %H:%M:%S")
                .ok()
                .map(|naive| DateTime::from_naive_utc_and_offset(naive, Utc))
        });

    Some(ExifMeta {
        taken,
        field_count,
        camera,
    })
}

/// `DateTimeOriginal` from EXIF, falling back to `DateTime` (IFD0).
pub fn exif_taken(path: &Path) -> Option<DateTime<Utc>> {
    exif_meta(path).and_then(|m| m.taken)
}

pub fn system_time_to_utc(t: SystemTime) -> Option<DateTime<Utc>> {
    Some(DateTime::<Utc>::from(t))
}

pub fn modified(path: &Path) -> Option<DateTime<Utc>> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()
        .and_then(system_time_to_utc)
}

pub fn created(path: &Path) -> Option<DateTime<Utc>> {
    std::fs::metadata(path)
        .ok()?
        .created()
        .ok()
        .and_then(system_time_to_utc)
}
