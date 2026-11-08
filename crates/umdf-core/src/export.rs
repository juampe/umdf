//! CSV and XML result exporters.
//!
//! The CSV header is `Group Num;Picture Filename;Similarity;`. The XML layout
//! uses `Summary`, `PicturesFound`, `SimilarPicturesFound`, `Duplicates`,
//! `Match`, `File`, `FileName`, `FileSize`, `Width` and `Height`.

use std::fmt::Write as _;
use std::path::Path;

use crate::model::{Group, MediaItem};

pub fn write_csv(path: &Path, items: &[MediaItem], groups: &[Group]) -> anyhow::Result<()> {
    let mut out = String::new();
    out.push_str("Group Num;Picture Filename;Similarity;\n");
    for g in groups {
        for id in &g.remove {
            let it = &items[*id];
            let _ = writeln!(
                out,
                "{};{};{:.3};",
                g.number,
                escape_csv(&it.path.to_string_lossy()),
                similarity_for(g, it.id)
            );
        }
    }
    std::fs::write(path, out)?;
    Ok(())
}

fn similarity_for(g: &Group, _id: usize) -> f32 {
    g.max_similarity
}

fn escape_csv(s: &str) -> String {
    if s.contains(';') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

pub fn write_xml(path: &Path, items: &[MediaItem], groups: &[Group]) -> anyhow::Result<()> {
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    out.push_str("<DuplicateFinder>\n");
    let pictures = groups.iter().map(|g| g.items.len()).sum::<usize>();
    let _ = writeln!(out, "  <Summary>");
    let _ = writeln!(out, "    <PicturesFound>{pictures}</PicturesFound>");
    let _ = writeln!(
        out,
        "    <SimilarPicturesFound>{}</SimilarPicturesFound>",
        groups.len()
    );
    let _ = writeln!(
        out,
        "    <Duplicates>{}</Duplicates>",
        groups.iter().map(|g| g.remove.len()).sum::<usize>()
    );
    let _ = writeln!(out, "  </Summary>");
    out.push_str("  <Duplicates>\n");
    for g in groups {
        for id in &g.remove {
            let it = &items[*id];
            let keep = &items[g.keep];
            out.push_str("    <Match>\n");
            let _ = writeln!(out, "      <Group>{}</Group>", g.number);
            let _ = writeln!(out, "      <Similarity>{:.3}</Similarity>", g.max_similarity);
            let _ = writeln!(out, "      <Keep>{}</Keep>", escape_xml(&keep.path.to_string_lossy()));
            write_file(&mut out, it);
            out.push_str("    </Match>\n");
        }
    }
    out.push_str("  </Duplicates>\n");
    out.push_str("</DuplicateFinder>\n");
    std::fs::write(path, out)?;
    Ok(())
}

fn write_file(out: &mut String, it: &MediaItem) {
    out.push_str("      <File>\n");
    let _ = writeln!(out, "        <FileName>{}</FileName>", escape_xml(&it.path.to_string_lossy()));
    let _ = writeln!(out, "        <FileSize>{}</FileSize>", it.size);
    let _ = writeln!(
        out,
        "        <Width>{}</Width>",
        it.width.map(|v| v.to_string()).unwrap_or_default()
    );
    let _ = writeln!(
        out,
        "        <Height>{}</Height>",
        it.height.map(|v| v.to_string()).unwrap_or_default()
    );
    let _ = writeln!(
        out,
        "        <DateTime>{}</DateTime>",
        it.effective_date().map(|d| d.to_rfc3339()).unwrap_or_default()
    );
    out.push_str("      </File>\n");
}

pub fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_escapes() {
        assert_eq!(escape_xml("a&b<c>"), "a&amp;b&lt;c&gt;");
    }

    #[test]
    fn csv_escapes_semicolons() {
        assert_eq!(escape_csv("a;b"), "\"a;b\"");
    }
}
