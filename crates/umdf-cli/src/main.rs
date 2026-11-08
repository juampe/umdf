//! `umdf` — headless Universal Media Duplicate Finder.
//!
//! Flags intentionally mirror the ones documented by the three analysed
//! programs so existing scripts keep working:
//!
//! ```text
//! umdf --folder "C:\Pictures" --selectby=filesize --moveto "C:\Duplicates" --apply
//! umdf --folder ~/Pictures --xml results.xml --csv results.csv
//! ```

use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use umdf_core::config::{
    default_audio_extensions, default_image_extensions, default_video_extensions, is_audio_ext,
    is_image_ext, is_video_ext,
};
use umdf_core::{Action, KeepPolicy, Metric, ScanConfig};

#[derive(Debug, Parser)]
#[command(
    name = "umdf",
    version,
    about = "Universal Media Duplicate Finder — find duplicate images, videos, audio and files",
    long_about = None
)]
struct Cli {
    /// Folder to scan. Repeatable.
    #[arg(long = "folder", value_name = "PATH")]
    folders: Vec<PathBuf>,

    /// Do not descend into sub-folders.
    #[arg(long)]
    no_recursive: bool,

    /// Comma separated extension allow-list (e.g. `jpg,png,mp4`).
    #[arg(long = "ext", alias = "types", value_name = "LIST")]
    extensions: Option<String>,

    /// Scan every file type we know about, including video and audio.
    #[arg(long)]
    all_media: bool,

    /// Also treat unknown extensions as plain files (exact-duplicate only).
    #[arg(long)]
    any_file: bool,

    #[arg(long, default_value_t = 0)]
    min_width: u32,

    #[arg(long, default_value_t = 0)]
    min_height: u32,

    #[arg(long, default_value_t = 0)]
    min_size: u64,

    /// Fingerprint grid edge (75 is the default resample size).
    #[arg(long, default_value_t = 75)]
    grid: u32,

    /// Similarity metric: dpf (pixel tolerance) | ssim | sign | pearson | legacy.
    #[arg(long, default_value = "dpf")]
    metric: String,

    /// Grey tolerance for the `dpf` metric (0..=255).
    #[arg(long, default_value_t = 10)]
    tolerance: u8,

    /// Minimum similarity (0..100) to report a pair.
    #[arg(long, default_value_t = 90.0)]
    threshold: f32,

    /// Only report byte-identical files.
    #[arg(long)]
    only_identical: bool,

    /// Compare decoded media content (re-muxed files match).
    #[arg(long)]
    compare_media_content: bool,

    /// Position in seconds used to grab the video frame.
    #[arg(long, default_value_t = 1.0)]
    frame_position: f64,

    /// Path to ffmpeg (ffprobe is looked up next to it).
    #[arg(long)]
    ffmpeg: Option<PathBuf>,

    /// Worker threads for decoding/hashing/comparing (0 = one per CPU).
    #[arg(long, default_value_t = 0)]
    threads: usize,

    /// How to choose the copy to keep:
    /// original (default) | filesize | dimensions | earlierdate |
    /// earlierexifdate | newest | oldest | name | path
    #[arg(long = "selectby", alias = "keep", default_value = "original")]
    selectby: String,

    /// Keep exactly one file per group after the selection policy.
    #[arg(long = "leave1")]
    leave1: bool,

    /// Send selected duplicates to the OS trash.
    #[arg(long)]
    trash: bool,

    /// Permanently delete selected duplicates.
    #[arg(long)]
    delete: bool,

    /// Move selected duplicates into this folder.
    #[arg(long = "moveto", value_name = "PATH")]
    moveto: Option<PathBuf>,

    /// Actually perform the selected action. Without it everything is a dry run.
    #[arg(long)]
    apply: bool,

    /// Write results as the AppFinder CSV.
    #[arg(long)]
    csv: Option<PathBuf>,

    /// Write results as the Picture-Finder XML.
    #[arg(long)]
    xml: Option<PathBuf>,

    /// Print the full report as JSON.
    #[arg(long)]
    json: bool,

    /// Only print the summary.
    #[arg(long)]
    quiet: bool,

    /// Accepted for compatibility with legacy scripts (the CLI always quits).
    #[arg(long)]
    quit: bool,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let cfg = build_config(&cli)?;

    if cfg.folders.is_empty() {
        anyhow::bail!("no --folder given (try --folder .)");
    }

    let report = umdf_core::scan(&cfg)?;

    // ---- actions ------------------------------------------------------------
    if let Some(actions) = &report.actions {
        println!(
            "actions: {} trashed, {} deleted, {} moved, {} failed ({} bytes freed)",
            actions.trashed, actions.deleted, actions.moved, actions.failed, actions.bytes_freed
        );
    } else if !matches!(cfg.action, Action::None) && !cfg.apply {
        println!(
            "dry run: {} file(s) would be processed. Add --apply to execute.",
            report.groups.iter().map(|g| g.remove.len()).sum::<usize>()
        );
    }

    // ---- exports ------------------------------------------------------------
    if let Some(path) = &cli.csv {
        umdf_core::export::write_csv(path, &report.items, &report.groups)
            .with_context(|| format!("writing {}", path.display()))?;
        if !cli.quiet {
            println!("CSV written to {}", path.display());
        }
    }
    if let Some(path) = &cli.xml {
        umdf_core::export::write_xml(path, &report.items, &report.groups)
            .with_context(|| format!("writing {}", path.display()))?;
        if !cli.quiet {
            println!("XML written to {}", path.display());
        }
    }

    // ---- report -------------------------------------------------------------
    if cli.json {
        let payload = serde_json::json!({
            "stats": {
                "files_scanned": report.stats.files_scanned,
                "files_hashed": report.stats.files_hashed,
                "files_fingerprinted": report.stats.files_fingerprinted,
                "pairs_compared": report.stats.pairs_compared,
                "duplicates_found": report.stats.duplicates_found,
                "groups": report.stats.groups,
                "total_bytes": report.stats.total_bytes,
                "reclaimable_bytes": report.stats.reclaimable_bytes,
                "elapsed_ms": report.stats.elapsed_ms,
                "threads": report.stats.threads,
                "extract_ms": report.stats.extract_ms,
                "hash_ms": report.stats.hash_ms,
                "compare_ms": report.stats.compare_ms,
                "group_ms": report.stats.group_ms,
            },
            "groups": report.groups.iter().map(|g| {
                serde_json::json!({
                    "number": g.number,
                    "max_similarity": g.max_similarity,
                    "keep": report.items[g.keep].path,
                    "keep_confidence": g.keep_confidence,
                    "keep_reasons": g.keep_reasons,
                    "remove": g.remove.iter().map(|id| report.items[*id].path.clone()).collect::<Vec<_>>(),
                    "reclaimable_bytes": g.reclaimable_bytes,
                })
            }).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    if !cli.quiet {
        println!(
            "scanned {} file(s), fingerprinted {}, hashed {}, compared {} pair(s) in {:.2}s with {} thread(s)",
            report.stats.files_scanned,
            report.stats.files_fingerprinted,
            report.stats.files_hashed,
            report.stats.pairs_compared,
            report.stats.elapsed_ms as f64 / 1000.0,
            report.stats.threads
        );
        println!(
            "found {} group(s), {} duplicate(s), {} reclaimable",
            report.stats.groups,
            report.stats.duplicates_found,
            human_bytes(report.stats.reclaimable_bytes)
        );

        for g in &report.groups {
            println!();
            let reasons = if g.keep_reasons.is_empty() {
                String::new()
            } else {
                let labels: Vec<&str> = g
                    .keep_reasons
                    .iter()
                    .map(|code| reason_label(code))
                    .collect();
                format!(" [{}]", labels.join(", "))
            };
            println!(
                "group {}  (similarity {:.1}%, keep = {} — original {:.0}%{})",
                g.number,
                g.max_similarity,
                report.items[g.keep].path.display(),
                g.keep_confidence,
                reasons
            );
            for id in &g.remove {
                let it = &report.items[*id];
                println!(
                    "   - {}  [{} , {}, {}]",
                    it.path.display(),
                    human_bytes(it.size),
                    it.dimensions_label(),
                    it.effective_date()
                        .map(|d| d.format("%Y-%m-%d").to_string())
                        .unwrap_or_else(|| "-".into())
                );
            }
        }
    }

    Ok(())
}

fn build_config(cli: &Cli) -> anyhow::Result<ScanConfig> {
    let mut cfg = ScanConfig::default();
    cfg.folders = cli.folders.clone();
    cfg.recursive = !cli.no_recursive;
    cfg.min_width = cli.min_width;
    cfg.min_height = cli.min_height;
    cfg.min_size = cli.min_size;
    cfg.grid = cli.grid.clamp(8, 512);
    cfg.pixel_tolerance = cli.tolerance;
    cfg.similarity_threshold = cli.threshold.clamp(0.0, 100.0);
    cfg.only_identical = cli.only_identical;
    cfg.compare_media_content = cli.compare_media_content;
    cfg.frame_position_secs = cli.frame_position;
    cfg.ffmpeg = cli.ffmpeg.clone().or_else(|| {
        umdf_core::media::find_tool(&["ffmpeg", "ffmpeg.exe"])
    });
    cfg.threads = cli.threads.min(256);

    cfg.metric = Metric::parse(&cli.metric)
        .ok_or_else(|| anyhow::anyhow!("unknown --metric '{}'", cli.metric))?;

    cfg.keep_policy = KeepPolicy::parse(&cli.selectby).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown --selectby '{}' (original|filesize|dimensions|earlierdate|earlierexifdate|newest|oldest|name|path)",
            cli.selectby
        )
    })?;
    cfg.leave_one = cli.leave1;

    if let Some(list) = &cli.extensions {
        cfg.extensions = list
            .split(',')
            .map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
    } else if cli.all_media {
        cfg.extensions = default_image_extensions()
            .into_iter()
            .chain(default_video_extensions())
            .chain(default_audio_extensions())
            .collect();
        cfg.compare_media_content = true;
    } else if cli.any_file {
        cfg.extensions = Vec::new();
    }

    // Actions are mutually exclusive.
    let mut actions = 0;
    if cli.trash {
        actions += 1;
    }
    if cli.delete {
        actions += 1;
    }
    if cli.moveto.is_some() {
        actions += 1;
    }
    if actions > 1 {
        anyhow::bail!("--trash, --delete and --moveto are mutually exclusive");
    }
    cfg.action = if cli.trash {
        Action::Trash
    } else if cli.delete {
        Action::Delete
    } else if let Some(dir) = &cli.moveto {
        Action::MoveTo(dir.clone())
    } else {
        Action::None
    };
    cfg.apply = cli.apply;
    Ok(cfg)
}

fn reason_label(code: &str) -> &'static str {
    match code {
        "resolution" => "higher resolution",
        "quality" => "better quality per pixel/bitrate",
        "age" => "older file date",
        "metadata" => "camera EXIF metadata",
        "name" => "name without copy markers",
        "location" => "typical original location",
        _ => "most consistent signal set",
    }
}

fn human_bytes(n: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let f = n as f64;
    if f >= GB {
        format!("{:.2} GB", f / GB)
    } else if f >= MB {
        format!("{:.2} MB", f / MB)
    } else if f >= KB {
        format!("{:.0} KB", f / KB)
    } else {
        format!("{n} B")
    }
}

// Re-exported so the helper predicates are not flagged as unused when the
// extension list is built from `--ext`.
#[allow(dead_code)]
fn classify_ext(ext: &str) -> &'static str {
    if is_image_ext(ext) {
        "image"
    } else if is_video_ext(ext) {
        "video"
    } else if is_audio_ext(ext) {
        "audio"
    } else {
        "other"
    }
}
