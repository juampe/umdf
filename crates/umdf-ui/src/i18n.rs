//! Tiny catalog-based i18n for the UI.
//!
//! Static labels, dynamic phrases and the Help text live in
//! `crates/umdf-ui/i18n/<lang>.json`. Adding a language means adding its JSON
//! file and one entry to `Lang`/`CATALOG_TEXTS`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::paths::display_path;
use crate::{HelpLine, Strings};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Es,
    Fr,
    De,
    Pt,
    It,
    Zh,
    Ja,
    Ko,
}

impl Lang {
    pub const ALL: [Lang; 9] = [
        Lang::En,
        Lang::Es,
        Lang::Fr,
        Lang::De,
        Lang::Pt,
        Lang::It,
        Lang::Zh,
        Lang::Ja,
        Lang::Ko,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn from_index(index: i32) -> Self {
        Self::ALL
            .get(index.max(0) as usize)
            .copied()
            .unwrap_or(Lang::En)
    }

    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Es => "es",
            Lang::Fr => "fr",
            Lang::De => "de",
            Lang::Pt => "pt",
            Lang::It => "it",
            Lang::Zh => "zh",
            Lang::Ja => "ja",
            Lang::Ko => "ko",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|lang| lang.code().eq_ignore_ascii_case(code))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Trash,
    Move,
    Delete,
}

#[derive(Debug, Clone, Copy)]
pub struct Tr {
    pub lang: Lang,
}

const CATALOG_TEXTS: [&str; 9] = [
    include_str!("../i18n/en.json"),
    include_str!("../i18n/es.json"),
    include_str!("../i18n/fr.json"),
    include_str!("../i18n/de.json"),
    include_str!("../i18n/pt.json"),
    include_str!("../i18n/it.json"),
    include_str!("../i18n/zh.json"),
    include_str!("../i18n/ja.json"),
    include_str!("../i18n/ko.json"),
];

static CATALOGS: [OnceLock<Catalog>; 9] = [const { OnceLock::new() }; 9];

fn catalog(lang: Lang) -> &'static Catalog {
    CATALOGS[lang.index()].get_or_init(|| {
        serde_json::from_str(CATALOG_TEXTS[lang.index()]).unwrap_or_else(|error| {
            panic!("invalid i18n catalog for {}: {error}", lang.code())
        })
    })
}

#[derive(Debug, Deserialize)]
struct Catalog {
    strings: StringsRaw,
    phrases: HashMap<String, String>,
    help: Vec<HelpRaw>,
}

#[derive(Debug, Deserialize)]
struct HelpRaw {
    text: String,
    kind: i32,
}

#[derive(Debug, Deserialize)]
struct StringsRaw {
    title: String,
    add_folder: String,
    remove: String,
    clear: String,
    options: String,
    start: String,
    scanning: String,
    stop: String,
    dark: String,
    light: String,
    language: String,
    help: String,
    help_title: String,
    close: String,
    locations: String,
    pictures: String,
    documents: String,
    home: String,
    computer: String,
    folders_none: String,
    remove_selected: String,
    clear_all: String,
    opt_scan_subfolders: String,
    opt_only_identical: String,
    opt_compare_media: String,
    opt_similarity: String,
    opt_keep: String,
    opt_metric: String,
    opt_threads: String,
    opt_semantic: String,
    semantic_model: String,
    models_manage: String,
    models_title: String,
    models_help: String,
    models_folder: String,
    models_capabilities: String,
    models_download: String,
    models_use: String,
    models_active: String,
    models_remove: String,
    models_accel: String,
    models_threshold: String,
    models_none: String,
    models_clear_cache: String,
    semantic_unavailable: String,
    opt_tolerance: String,
    trash_all: String,
    move_all: String,
    delete_all: String,
    keeps_best: String,
    cancel: String,
    confirm: String,
    match_list: String,
    col_group: String,
    col_original: String,
    col_duplicate: String,
    col_similarity: String,
    col_size: String,
    empty_list: String,
    preview: String,
    original: String,
    duplicate: String,
    move_destination: String,
    move_placeholder: String,
    browse: String,
    trash: String,
    #[serde(rename = "move")]
    r#move: String,
    delete: String,
    select_folder: String,
    up: String,
    go: String,
    select_this_folder: String,
    show_hidden: String,
}

impl StringsRaw {
    fn to_slint(&self) -> Strings {
        Strings {
            title: self.title.clone().into(),
            add_folder: self.add_folder.clone().into(),
            remove: self.remove.clone().into(),
            clear: self.clear.clone().into(),
            options: self.options.clone().into(),
            start: self.start.clone().into(),
            scanning: self.scanning.clone().into(),
            stop: self.stop.clone().into(),
            dark: self.dark.clone().into(),
            light: self.light.clone().into(),
            language: self.language.clone().into(),
            help: self.help.clone().into(),
            help_title: self.help_title.clone().into(),
            close: self.close.clone().into(),
            locations: self.locations.clone().into(),
            pictures: self.pictures.clone().into(),
            documents: self.documents.clone().into(),
            home: self.home.clone().into(),
            computer: self.computer.clone().into(),
            folders_none: self.folders_none.clone().into(),
            remove_selected: self.remove_selected.clone().into(),
            clear_all: self.clear_all.clone().into(),
            opt_scan_subfolders: self.opt_scan_subfolders.clone().into(),
            opt_only_identical: self.opt_only_identical.clone().into(),
            opt_compare_media: self.opt_compare_media.clone().into(),
            opt_similarity: self.opt_similarity.clone().into(),
            opt_keep: self.opt_keep.clone().into(),
            opt_metric: self.opt_metric.clone().into(),
            opt_threads: self.opt_threads.clone().into(),
            opt_semantic: self.opt_semantic.clone().into(),
            semantic_model: self.semantic_model.clone().into(),
            models_manage: self.models_manage.clone().into(),
            models_title: self.models_title.clone().into(),
            models_help: self.models_help.clone().into(),
            models_folder: self.models_folder.clone().into(),
            models_capabilities: self.models_capabilities.clone().into(),
            models_download: self.models_download.clone().into(),
            models_use: self.models_use.clone().into(),
            models_active: self.models_active.clone().into(),
            models_remove: self.models_remove.clone().into(),
            models_accel: self.models_accel.clone().into(),
            models_threshold: self.models_threshold.clone().into(),
            models_none: self.models_none.clone().into(),
            models_clear_cache: self.models_clear_cache.clone().into(),
            semantic_unavailable: self.semantic_unavailable.clone().into(),
            opt_tolerance: self.opt_tolerance.clone().into(),
            trash_all: self.trash_all.clone().into(),
            move_all: self.move_all.clone().into(),
            delete_all: self.delete_all.clone().into(),
            keeps_best: self.keeps_best.clone().into(),
            cancel: self.cancel.clone().into(),
            confirm: self.confirm.clone().into(),
            match_list: self.match_list.clone().into(),
            col_group: self.col_group.clone().into(),
            col_original: self.col_original.clone().into(),
            col_duplicate: self.col_duplicate.clone().into(),
            col_similarity: self.col_similarity.clone().into(),
            col_size: self.col_size.clone().into(),
            empty_list: self.empty_list.clone().into(),
            preview: self.preview.clone().into(),
            original: self.original.clone().into(),
            duplicate: self.duplicate.clone().into(),
            move_destination: self.move_destination.clone().into(),
            move_placeholder: self.move_placeholder.clone().into(),
            browse: self.browse.clone().into(),
            trash: self.trash.clone().into(),
            r#move: self.r#move.clone().into(),
            delete: self.delete.clone().into(),
            select_folder: self.select_folder.clone().into(),
            up: self.up.clone().into(),
            go: self.go.clone().into(),
            select_this_folder: self.select_this_folder.clone().into(),
            show_hidden: self.show_hidden.clone().into(),
        }
    }
}


impl Tr {
    pub fn new(lang: Lang) -> Self {
        Self { lang }
    }

    pub fn strings(&self) -> Strings {
        catalog(self.lang).strings.to_slint()
    }

    /// Formatted help content: 0 = heading, 1 = paragraph, 2 = bullet.
    pub fn help_lines(&self) -> Vec<HelpLine> {
        let mut lines: Vec<HelpLine> = catalog(self.lang)
            .help
            .iter()
            .map(|line| HelpLine {
                text: line.text.clone().into(),
                kind: line.kind,
            })
            .collect();
        lines.push(HelpLine {
            text: self.phrase("help_about").into(),
            kind: 0,
        });
        lines.push(HelpLine {
            text: self
                .fmt(
                    "help_version",
                    &[("version", env!("CARGO_PKG_VERSION").to_string())],
                )
                .into(),
            kind: 1,
        });
        lines.push(HelpLine {
            text: self.phrase("help_author").into(),
            kind: 1,
        });
        lines.push(HelpLine {
            text: self.phrase("help_disclaimer").into(),
            kind: 1,
        });
        lines
    }

    fn phrase(&self, key: &'static str) -> &'static str {
        catalog(self.lang)
            .phrases
            .get(key)
            .or_else(|| catalog(Lang::En).phrases.get(key))
            .map(|value| value.as_str())
            .unwrap_or(key)
    }

    fn fmt(&self, key: &'static str, args: &[(&str, String)]) -> String {
        let mut text = self.phrase(key).to_string();
        for (name, value) in args {
            text = text.replace(&format!("{{{name}}}"), value);
        }
        text
    }

    fn verb_label(&self, verb: Verb) -> String {
        self.phrase(match verb {
            Verb::Trash => "verb_trash",
            Verb::Move => "verb_move",
            Verb::Delete => "verb_delete",
        })
        .to_string()
    }

    pub fn folders_label(&self, n: usize) -> String {
        self.fmt("folders_label", &[("n", n.to_string())])
    }

    pub fn stage_ready(&self) -> String {
        self.phrase("stage_ready").into()
    }

    pub fn stage_starting(&self) -> String {
        self.phrase("stage_starting").into()
    }

    pub fn stage_loading_total(&self, total: usize) -> String {
        self.fmt("stage_loading_total", &[("total", total.to_string())])
    }

    pub fn stage_loading(&self, name: &str) -> String {
        self.fmt("stage_loading", &[("name", name.to_string())])
    }

    pub fn stage_embedding(&self, done: usize, total: usize) -> String {
        self.fmt(
            "stage_embedding",
            &[("done", done.to_string()), ("total", total.to_string())],
        )
    }

    pub fn stage_hashing(&self, done: usize, total: usize) -> String {
        self.fmt(
            "stage_hashing",
            &[("done", done.to_string()), ("total", total.to_string())],
        )
    }

    pub fn stage_comparing(&self, done: usize, total: usize) -> String {
        self.fmt(
            "stage_comparing",
            &[("done", done.to_string()), ("total", total.to_string())],
        )
    }

    pub fn stage_grouping(&self) -> String {
        self.phrase("stage_grouping").into()
    }

    pub fn stage_finished(&self) -> String {
        self.phrase("stage_finished").into()
    }

    pub fn stage_stopping(&self) -> String {
        self.phrase("stage_stopping").into()
    }

    pub fn stage_stopped(&self) -> String {
        self.phrase("stage_stopped").into()
    }

    pub fn stage_error(&self) -> String {
        self.phrase("stage_error").into()
    }

    pub fn stage_no_folders(&self) -> String {
        self.phrase("stage_no_folders").into()
    }

    pub fn status_ready(&self) -> String {
        self.phrase("status_ready").into()
    }

    pub fn status_scanning(&self) -> String {
        self.phrase("status_scanning").into()
    }

    pub fn status_busy(&self) -> String {
        self.phrase("status_busy").into()
    }

    pub fn status_no_folders(&self) -> String {
        self.phrase("status_no_folders").into()
    }

    pub fn status_stopping(&self) -> String {
        self.phrase("status_stopping").into()
    }

    pub fn status_ffmpeg_missing(&self) -> String {
        self.phrase("status_ffmpeg_missing").into()
    }

    pub fn status_folder_added(&self, path: &Path) -> String {
        self.fmt("status_folder_added", &[("path", display_path(path))])
    }

    pub fn status_folder_exists(&self) -> String {
        self.phrase("status_folder_exists").into()
    }

    pub fn status_not_a_folder(&self, path: &Path) -> String {
        self.fmt("status_not_a_folder", &[("path", display_path(path))])
    }

    pub fn status_folder_removed(&self) -> String {
        self.phrase("status_folder_removed").into()
    }

    pub fn status_folders_cleared(&self) -> String {
        self.phrase("status_folders_cleared").into()
    }

    pub fn status_move_dest_missing(&self) -> String {
        self.phrase("status_move_dest_missing").into()
    }

    pub fn status_nothing_to_apply(&self) -> String {
        self.phrase("status_nothing_to_apply").into()
    }

    pub fn status_confirm_bulk(&self) -> String {
        self.phrase("status_confirm_bulk").into()
    }

    pub fn status_bulk_cancelled(&self) -> String {
        self.phrase("status_bulk_cancelled").into()
    }

    pub fn status_trashed(&self, path: &Path) -> String {
        self.fmt("status_trashed", &[("path", display_path(path))])
    }

    pub fn status_trash_failed(&self, path: &Path) -> String {
        self.fmt("status_trash_failed", &[("path", display_path(path))])
    }

    pub fn status_deleted(&self, path: &Path) -> String {
        self.fmt("status_deleted", &[("path", display_path(path))])
    }

    pub fn status_delete_failed(&self, path: &Path) -> String {
        self.fmt("status_delete_failed", &[("path", display_path(path))])
    }

    pub fn status_moved(&self, path: &Path) -> String {
        self.fmt("status_moved", &[("path", display_path(path))])
    }

    pub fn status_move_failed(&self, path: &Path) -> String {
        self.fmt("status_move_failed", &[("path", display_path(path))])
    }

    pub fn status_error(&self, error: &str) -> String {
        self.fmt("status_error", &[("error", error.to_string())])
    }

    pub fn confirm_bulk(&self, verb: Verb, count: usize, destination: Option<&Path>) -> String {
        let verb = self.verb_label(verb);
        let count = count.to_string();
        match destination {
            Some(destination) => self.fmt(
                "confirm_bulk_dest",
                &[
                    ("verb", verb),
                    ("count", count),
                    ("destination", display_path(destination)),
                ],
            ),
            None => self.fmt("confirm_bulk", &[("verb", verb), ("count", count)]),
        }
    }

    pub fn bulk_done(&self, done: usize, verb: Verb, bytes: &str, failed: usize) -> String {
        let verb = self.verb_label(verb);
        self.fmt(
            "bulk_done",
            &[
                ("verb", verb),
                ("done", done.to_string()),
                ("bytes", bytes.to_string()),
                ("failed", failed.to_string()),
            ],
        )
    }

    pub fn files_scanned(&self, n: usize) -> String {
        self.fmt("files_scanned", &[("n", n.to_string())])
    }

    pub fn files_scanned_progress(&self, index: usize, total: usize) -> String {
        self.fmt(
            "files_scanned_progress",
            &[("index", index.to_string()), ("total", total.to_string())],
        )
    }

    pub fn duplicates_found(&self, n: usize) -> String {
        self.fmt("duplicates_found", &[("n", n.to_string())])
    }

    pub fn duplicates_found_groups(&self, n: usize, groups: usize) -> String {
        self.fmt(
            "duplicates_found_groups",
            &[("n", n.to_string()), ("groups", groups.to_string())],
        )
    }

    pub fn reclaimable(&self, human: &str) -> String {
        self.fmt("reclaimable", &[("human", human.to_string())])
    }

    pub fn elapsed(&self, secs: f64, groups: usize, threads: usize) -> String {
        self.fmt(
            "elapsed",
            &[
                ("secs", format!("{secs:.1}")),
                ("groups", groups.to_string()),
                ("threads", threads.to_string()),
            ],
        )
    }

    pub fn original_likely(&self, pct: f32) -> String {
        self.fmt("original_likely", &[("pct", format!("{pct:.0}"))])
    }

    pub fn reason_labels(&self, codes: &[String]) -> String {
        codes
            .iter()
            .map(|code| {
                let key = match code.as_str() {
                    "resolution" => "reason_resolution",
                    "quality" => "reason_quality",
                    "age" => "reason_age",
                    "metadata" => "reason_metadata",
                    "name" => "reason_name",
                    "location" => "reason_location",
                    _ => "reason_other",
                };
                self.phrase(key)
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn no_picture(&self) -> String {
        self.phrase("no_picture").into()
    }

    pub fn status_stopped_partial(&self, n: usize) -> String {
        self.fmt("status_stopped_partial", &[("n", n.to_string())])
    }

    pub fn status_stopped_zero(&self) -> String {
        self.phrase("status_stopped_zero").into()
    }

    pub fn status_finished(&self, groups: usize, duplicates: usize) -> String {
        self.fmt(
            "status_finished",
            &[
                ("groups", groups.to_string()),
                ("duplicates", duplicates.to_string()),
            ],
        )
    }

    pub fn browser_folders(&self, n: usize) -> String {
        self.fmt("browser_folders", &[("n", n.to_string())])
    }

    pub fn browser_drives(&self) -> String {
        self.phrase("browser_drives").into()
    }

    pub fn browser_drives_hint(&self, n: usize) -> String {
        self.fmt("browser_drives_hint", &[("n", n.to_string())])
    }

    pub fn model_no_url(&self) -> String {
        self.phrase("model_no_url").into()
    }

    pub fn model_downloading(&self, name: &str) -> String {
        self.fmt("model_downloading", &[("name", name.to_string())])
    }

    pub fn model_downloaded(&self, name: &str, mb: f64) -> String {
        self.fmt(
            "model_downloaded",
            &[("name", name.to_string()), ("mb", format!("{mb:.0}"))],
        )
    }

    pub fn model_failed(&self, error: &str) -> String {
        self.fmt("model_failed", &[("error", error.to_string())])
    }

    pub fn model_missing(&self, path: &str) -> String {
        self.fmt("model_missing", &[("path", path.to_string())])
    }

    pub fn model_active(&self, name: &str) -> String {
        self.fmt("model_active", &[("name", name.to_string())])
    }

    pub fn model_removed(&self, name: &str) -> String {
        self.fmt("model_removed", &[("name", name.to_string())])
    }

    pub fn models_folder(&self, path: &str) -> String {
        self.fmt("models_folder", &[("path", path.to_string())])
    }

    pub fn cache_cleared(&self) -> String {
        self.phrase("cache_cleared").into()
    }

    pub fn no_embeddings_build(&self) -> String {
        self.phrase("no_embeddings_build").into()
    }

    pub fn model_select_first(&self) -> String {
        self.phrase("model_select_first").into()
    }

    pub fn browser_not_a_folder(&self, path: &Path) -> String {
        self.fmt(
            "browser_not_a_folder",
            &[("path", display_path(path))],
        )
    }

    pub fn browser_destination(&self) -> String {
        self.phrase("browser_destination").into()
    }

    pub fn model_name(&self, id: &str) -> Option<String> {
        match id {
            "clip" => Some(self.phrase("model_clip_name").to_string()),
            "siglip" => Some(self.phrase("model_siglip_name").to_string()),
            _ => None,
        }
    }

    pub fn model_capabilities(&self, id: &str) -> String {
        let key = match id {
            "resnet18" => "model_resnet18_caps",
            "resnet50" => "model_resnet50_caps",
            "clip" => "model_clip_caps",
            "siglip" => "model_siglip_caps",
            _ => return String::new(),
        };
        self.phrase(key).to_string()
    }

    pub fn no_model_selected(&self) -> String {
        self.phrase("no_model_selected").into()
    }

    pub fn keep_labels(&self) -> Vec<String> {
        [
            "keep_original",
            "keep_dimensions",
            "keep_filesize",
            "keep_earlierdate",
            "keep_earlierexifdate",
            "keep_oldest",
            "keep_name",
            "keep_path",
        ]
        .into_iter()
        .map(|key| self.phrase(key).to_string())
        .collect()
    }

    pub fn thread_labels(&self) -> Vec<String> {
        let mut labels = vec![self.phrase("auto_label").to_string()];
        labels.extend(["1", "2", "4", "8", "16", "32"].into_iter().map(str::to_string));
        labels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_parses_and_has_content() {
        for lang in Lang::ALL {
            let catalog = catalog(lang);
            assert!(!catalog.strings.title.is_empty(), "{}", lang.code());
            assert!(catalog.help.len() > 10, "{}", lang.code());
            for key in ["folders_label", "stage_ready", "confirm_bulk", "keep_original"] {
                assert!(
                    catalog.phrases.contains_key(key),
                    "{} missing {key}",
                    lang.code()
                );
            }
        }
    }

    #[test]
    fn help_includes_about_version_author_and_disclaimer() {
        let lines = Tr::new(Lang::En).help_lines();
        let text = lines
            .iter()
            .map(|line| line.text.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains(env!("CARGO_PKG_VERSION")));
        assert!(text.contains("Juan Pedro Paredes Caballero"));
        assert!(text.to_lowercase().contains("disclaimer"));
    }

    #[test]
    fn language_codes_round_trip() {
        for lang in Lang::ALL {
            assert_eq!(Lang::from_code(lang.code()), Some(lang));
            assert_eq!(Lang::from_index(lang.index() as i32), lang);
        }
    }
}

pub fn human_bytes(n: u64) -> String {
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
