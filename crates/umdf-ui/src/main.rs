//! Slint desktop front-end for the Universal Media Duplicate Finder.

mod cache;
mod i18n;
mod models;
mod settings;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};

use slint::{ComponentHandle, Model, ModelRc, SharedPixelBuffer, VecModel};
use umdf_core::config::{
    default_audio_extensions, default_image_extensions, default_video_extensions,
};
use umdf_core::{EmbeddingConfig, KeepPolicy, Metric, Progress, ScanConfig, ScanReport};

use cache::CacheDb;
use i18n::{human_bytes, Lang, Tr, Verb};
use models::InstalledModel;
use settings::Settings;

slint::include_modules!();

/// Must match the ComboBox model in `ui/app.slint`:
/// original | dimensions | filesize | earlierdate | earlierexifdate |
/// oldest | name | path
const KEEP_POLICIES: &[KeepPolicy] = &[
    KeepPolicy::MostLikelyOriginal,
    KeepPolicy::HighestResolution,
    KeepPolicy::Largest,
    KeepPolicy::Newest,
    KeepPolicy::Newest,
    KeepPolicy::Oldest,
    KeepPolicy::FirstName,
    KeepPolicy::ShortestPath,
];

/// ComboBox order in `ui/app.slint` (0 = one per CPU).
const THREAD_OPTIONS: &[usize] = &[0, 1, 2, 4, 8, 16, 32];

const METRICS: &[Metric] = &[
    Metric::PixelTolerance,
    Metric::Ssim,
    Metric::NormalizedSignAgreement,
    Metric::Pearson,
    Metric::LegacySignAgreement,
];

/// Above this many rows we skip thumbnails to keep the UI responsive.
const MAX_THUMBNAIL_ROWS: usize = 300;

/// Raw RGBA thumbnail/preview data (definitely `Send`, unlike `slint::Image`).
type ThumbData = (u32, u32, Vec<u8>);
type PreviewData = (u32, u32, Vec<u8>, String);

#[derive(Clone)]
struct UiModel {
    name: String,
    path: PathBuf,
    url: String,
    capabilities: String,
    preset: String,
    input_size: u32,
    threshold: f32,
}

/// Live counters shown in the status bar. Kept in sync with deletions so a
/// language change does not restore stale values.
#[derive(Clone, Copy, Default)]
struct UiStats {
    scanned: usize,
    found: usize,
    groups: usize,
    reclaimable: u64,
    elapsed_ms: u128,
    threads: usize,
}

fn main() -> anyhow::Result<()> {
    let ui = AppWindow::new()?;
    // Initial size set once; the window stays resizable/maximizable because the
    // size is not bound in the .slint file.
    ui.window()
        .set_size(slint::LogicalSize::new(1280.0, 820.0));

    let lang = Rc::new(Cell::new(Lang::En));
    let auto_threads = umdf_core::scan::effective_threads(&ScanConfig::default());
    let boot = Tr::new(Lang::En);
    ui.set_strings(boot.strings().into());
    ui.set_help_lines(ModelRc::from(Rc::new(VecModel::from(boot.help_lines()))));
    ui.set_embeddings_supported(cfg!(feature = "embeddings"));

    // ---- persisted configuration (config.toml) + cache (cache.db) -----------
    let settings: Arc<Mutex<Settings>> = Arc::new(Mutex::new(Settings::load()));
    let cache: Arc<CacheDb> = Arc::new(match CacheDb::open() {
        Ok(cache) => cache,
        Err(error) => {
            eprintln!("UMDF: could not open cache.db: {error}");
            let fallback = std::env::temp_dir().join("umdf-cache.db");
            CacheDb::open_at(fallback).expect("temporary cache database")
        }
    });
    let ui_models: Arc<Mutex<Vec<UiModel>>> = Arc::new(Mutex::new(Vec::new()));

    let (models_dir, device, threshold, active_model, installed) = {
        let guard = settings.lock().expect("settings lock");
        (
            guard.models_dir.clone(),
            guard.device_index(),
            guard.similarity,
            guard.active_model.clone(),
            guard.models.clone(),
        )
    };

    update_cache_label(&ui, &cache);
    ui.set_models_dir(models_dir.to_string_lossy().to_string().into());
    ui.set_semantic_device(device);
    ui.set_semantic_threshold(threshold);
    ui.set_semantic_threshold_label(format!("{threshold:.2}").into());

    if !active_model.is_empty() && Path::new(&active_model).is_file() {
        ui.set_semantic_enabled(true);
        ui.set_semantic_model(active_model.clone().into());
        if let Some(found) = installed
            .iter()
            .find(|model| model.path == PathBuf::from(&active_model))
        {
            ui.set_semantic_preset(if found.preset == "clip" { 1 } else { 0 });
            ui.set_semantic_input_size(found.input_size as i32);
            ui.set_semantic_threshold(found.threshold);
            ui.set_semantic_threshold_label(format!("{:.2}", found.threshold).into());
        }
    }
    refresh_models(&ui, &settings, &ui_models, lang.get() == Lang::Es);

    ui.set_preview_left_info(boot.no_picture().into());
    ui.set_preview_right_info(boot.no_picture().into());
    ui.set_folders_label(boot.folders_label(0).into());
    ui.set_scanned_label(boot.files_scanned(0).into());
    ui.set_found_label(boot.duplicates_found(0).into());
    ui.set_space_label(boot.reclaimable(&human_bytes(0)).into());
    ui.set_elapsed_label(boot.elapsed(0.0, 0, auto_threads).into());

    let rows: Rc<VecModel<RowItem>> = Rc::new(VecModel::default());
    let folder_model: Rc<VecModel<FolderItem>> = Rc::new(VecModel::default());
    let browser_model: Rc<VecModel<BrowserEntry>> = Rc::new(VecModel::default());
    ui.set_rows(ModelRc::from(rows.clone()));
    ui.set_folders(ModelRc::from(folder_model.clone()));
    ui.set_browser_entries(ModelRc::from(browser_model.clone()));

    let folders: Rc<RefCell<Vec<PathBuf>>> = Rc::new(RefCell::new(Vec::new()));
    let browser_path: Rc<RefCell<PathBuf>> = Rc::new(RefCell::new(current_dir()));
    let cancel = Arc::new(AtomicBool::new(false));
    let ui_stats: Rc<RefCell<UiStats>> = Rc::new(RefCell::new(UiStats {
        threads: auto_threads,
        ..Default::default()
    }));
    let thumbs: Arc<Mutex<HashMap<String, Option<ThumbData>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let thumb_gen = Arc::new(AtomicU64::new(0));
    let preview_cache: Arc<Mutex<HashMap<String, Option<PreviewData>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let preview_gen = Arc::new(AtomicU64::new(0));

    let (ptx, prx) = channel::<Progress>();
    let (dtx, drx) = channel::<Result<ScanReport, String>>();
    let prx = Rc::new(RefCell::new(prx));
    let drx = Rc::new(RefCell::new(drx));

    // ---- language -----------------------------------------------------------
    {
        let ui_handle = ui.as_weak();
        let lang = lang.clone();
        let folders = folders.clone();
        let rows = rows.clone();
        let ui_stats = ui_stats.clone();
        let settings = settings.clone();
        let ui_models = ui_models.clone();
        ui.on_set_lang(move |index| {
            let Some(ui) = ui_handle.upgrade() else { return };
            let new_lang = Lang::from_index(index);
            lang.set(new_lang);
            let tr = Tr::new(new_lang);
            ui.set_strings(tr.strings());
            ui.set_help_lines(ModelRc::from(Rc::new(VecModel::from(tr.help_lines()))));
            ui.set_folders_label(tr.folders_label(folders.borrow().len()).into());
            refresh_models(&ui, &settings, &ui_models, new_lang == Lang::Es);

            // Counters come from the live state (deletions included), not from
            // the original report.
            let stats = *ui_stats.borrow();
            ui.set_scanned_label(tr.files_scanned(stats.scanned).into());
            ui.set_found_label(tr.duplicates_found(stats.found).into());
            ui.set_space_label(tr.reclaimable(&human_bytes(stats.reclaimable)).into());
            ui.set_elapsed_label(
                tr.elapsed(
                    stats.elapsed_ms as f64 / 1000.0,
                    stats.groups,
                    stats.threads,
                )
                .into(),
            );

            // Re-translate the visible rows in place. Rebuilding from the scan
            // report would resurrect rows the user already deleted.
            for i in 0..rows.row_count() {
                if let Some(mut row) = rows.row_data(i) {
                    let pct = row.keep_pct;
                    let codes: Vec<String> = row
                        .keep_codes
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect();
                    row.keep_confidence = tr.original_likely(pct).into();
                    row.keep_reason = tr.reason_labels(&codes).into();
                    rows.set_row_data(i, row);
                }
            }

            if !ui.get_busy() {
                ui.set_stage(
                    if stats.scanned > 0 {
                        tr.stage_finished()
                    } else {
                        tr.stage_ready()
                    }
                    .into(),
                );
                ui.set_status(tr.status_ready().into());
            }
            if ui.get_selected_row() >= 0 {
                if let Some(row) = rows.row_data(ui.get_selected_row() as usize) {
                    ui.set_keep_confidence(row.keep_confidence.clone());
                    ui.set_keep_reason(row.keep_reason.clone());
                    ui.set_preview_left_info(row_original_info(&row));
                    ui.set_preview_right_info(row_duplicate_info(&row));
                }
            }
        });
    }

    // ---- theme --------------------------------------------------------------
    {
        let ui_handle = ui.as_weak();
        ui.on_toggle_theme(move || {
            if let Some(ui) = ui_handle.upgrade() {
                let next = !ui.get_dark_mode();
                ui.set_dark_mode(next);
            }
        });
    }

    // ---- folder browser -----------------------------------------------------
    {
        let ui_handle = ui.as_weak();
        let browser_model = browser_model.clone();
        let browser_path = browser_path.clone();
        let folders = folders.clone();
        let lang = lang.clone();
        ui.on_open_browser(move |key| {
            let Some(ui) = ui_handle.upgrade() else { return };
            let tr = Tr::new(lang.get());
            if key == "models-dir" {
                ui.set_browser_mode("models-dir".into());
                let start = {
                    let dir = PathBuf::from(ui.get_models_dir().to_string());
                    if dir.is_dir() {
                        dir
                    } else {
                        quick_location("home").unwrap_or_else(current_dir)
                    }
                };
                open_browser(&ui, &browser_model, &browser_path, start, &tr);
                return;
            }
            if key == "destination" {
                ui.set_browser_mode("destination".into());
                let start = {
                    let dest = PathBuf::from(ui.get_move_dest().to_string());
                    if dest.is_dir() {
                        dest
                    } else {
                        folders
                            .borrow()
                            .last()
                            .cloned()
                            .or_else(|| quick_location("home"))
                            .unwrap_or_else(current_dir)
                    }
                };
                open_browser(&ui, &browser_model, &browser_path, start, &tr);
                return;
            }
            ui.set_browser_mode("folder".into());
            let start = match key.as_str() {
                "pictures" => quick_location("pictures"),
                "documents" => quick_location("documents"),
                "home" => quick_location("home"),
                "root" => quick_location("root"),
                _ => folders
                    .borrow()
                    .last()
                    .cloned()
                    .or_else(|| quick_location("home"))
                    .or_else(|| Some(current_dir())),
            };
            match start {
                Some(path) => open_browser(&ui, &browser_model, &browser_path, path, &tr),
                None => ui.set_status(tr.status_not_a_folder(Path::new("/")).into()),
            }
        });
    }
    {
        let ui_handle = ui.as_weak();
        let browser_model = browser_model.clone();
        let browser_path = browser_path.clone();
        let lang = lang.clone();
        ui.on_browse_go(move |value| {
            let Some(ui) = ui_handle.upgrade() else { return };
            let tr = Tr::new(lang.get());
            let raw = if value.trim().is_empty() {
                ui.get_browser_path().to_string()
            } else {
                value.to_string()
            };
            let path = PathBuf::from(raw);
            if path.is_dir() {
                open_browser(&ui, &browser_model, &browser_path, path, &tr);
            } else {
                ui.set_browser_status(tr.browser_not_a_folder(&path).into());
            }
        });
    }
    {
        let ui_handle = ui.as_weak();
        let browser_model = browser_model.clone();
        let browser_path = browser_path.clone();
        let lang = lang.clone();
        ui.on_browse_up(move || {
            let Some(ui) = ui_handle.upgrade() else { return };
            let tr = Tr::new(lang.get());
            let current = browser_path.borrow().clone();
            if let Some(parent) = current.parent() {
                open_browser(&ui, &browser_model, &browser_path, parent.to_path_buf(), &tr);
            }
        });
    }
    {
        let ui_handle = ui.as_weak();
        let browser_model = browser_model.clone();
        let browser_path = browser_path.clone();
        let lang = lang.clone();
        ui.on_browse_enter(move |value| {
            let Some(ui) = ui_handle.upgrade() else { return };
            let tr = Tr::new(lang.get());
            let path = PathBuf::from(value.to_string());
            if path.is_dir() {
                open_browser(&ui, &browser_model, &browser_path, path, &tr);
            }
        });
    }
    {
        let ui_handle = ui.as_weak();
        let browser_path = browser_path.clone();
        let folders = folders.clone();
        let folder_model = folder_model.clone();
        let lang = lang.clone();
        let settings = settings.clone();
        let ui_models = ui_models.clone();
        ui.on_browse_select(move || {
            let Some(ui) = ui_handle.upgrade() else { return };
            let tr = Tr::new(lang.get());
            let path = browser_path.borrow().clone();
            if ui.get_browser_mode() == "models-dir" {
                if path.is_dir() {
                    ui.set_models_dir(path.to_string_lossy().to_string().into());
                    save_model_settings(&ui, &settings);
                    refresh_models(&ui, &settings, &ui_models, lang.get() == Lang::Es);
                    ui.set_models_status(
                        Tr::new(lang.get())
                            .models_folder(&path.display().to_string())
                            .into(),
                    );
                }
                ui.set_browser_open(false);
                return;
            }
            if ui.get_browser_mode() == "destination" {
                if path.is_dir() {
                    ui.set_move_dest(path.to_string_lossy().to_string().into());
                    ui.set_status(tr.status_folder_added(&path).into());
                }
            } else {
                add_folder(&ui, &folder_model, &folders, path, &tr);
            }
            ui.set_browser_open(false);
        });
    }
    {
        let ui_handle = ui.as_weak();
        ui.on_browse_cancel(move || {
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_browser_open(false);
            }
        });
    }

    // ---- show hidden --------------------------------------------------------
    {
        let ui_handle = ui.as_weak();
        let browser_model = browser_model.clone();
        let browser_path = browser_path.clone();
        let lang = lang.clone();
        ui.on_toggle_hidden(move |show| {
            let Some(ui) = ui_handle.upgrade() else { return };
            ui.set_show_hidden(show);
            let current = browser_path.borrow().clone();
            open_browser(
                &ui,
                &browser_model,
                &browser_path,
                current,
                &Tr::new(lang.get()),
            );
        });
    }

    {
        let ui_handle = ui.as_weak();
        let settings = settings.clone();
        let ui_models = ui_models.clone();
        let cache = cache.clone();
        let lang = lang.clone();
        ui.on_open_models(move || {
            let Some(ui) = ui_handle.upgrade() else { return };
            refresh_models(&ui, &settings, &ui_models, lang.get() == Lang::Es);
            update_cache_label(&ui, &cache);
            ui.set_models_open(true);
        });
    }
    {
        let ui_handle = ui.as_weak();
        let cache = cache.clone();
        let lang = lang.clone();
        ui.on_clear_cache(move || {
            let Some(ui) = ui_handle.upgrade() else { return };
            let tr = Tr::new(lang.get());
            match cache.clear() {
                Ok(()) => {
                    ui.set_models_status(tr.cache_cleared().into());
                    update_cache_label(&ui, &cache);
                }
                Err(error) => {
                    ui.set_models_status(tr.model_failed(&error.to_string()).into());
                }
            }
        });
    }

    // ---- model manager ------------------------------------------------------
    {
        let ui_handle = ui.as_weak();
        let settings = settings.clone();
        let ui_models = ui_models.clone();
        let lang = lang.clone();
        ui.on_download_model(move |index| {
            let tr = Tr::new(lang.get());
            let Some(ui) = ui_handle.upgrade() else { return };
            let Some(model) = ui_models.lock().ok().and_then(|guard| guard.get(index as usize).cloned()) else { return };
            if model.url.is_empty() {
                ui.set_models_status(tr.model_no_url().into());
                return;
            }
            ui.set_models_status(tr.model_downloading(&model.name).into());
            let lang_es = lang.get() == Lang::Es;
            let ui_weak = ui_handle.clone();
            let settings = settings.clone();
            let ui_models = ui_models.clone();
            std::thread::spawn(move || {
                let result = models::download(&model.url, &model.path);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_weak.upgrade() else { return };
                    match result {
                        Ok(bytes) => {
                            let (size, preset) = detect_model(&model);
                            let installed = InstalledModel {
                                name: model.name.clone(),
                                path: model.path.clone(),
                                capabilities: model.capabilities.clone(),
                                preset,
                                input_size: size,
                                threshold: model.threshold,
                                source_url: model.url.clone(),
                            };
                            if let Ok(mut guard) = settings.lock() {
                                guard.upsert_model(installed);
                                let _ = guard.save();
                            }
                            refresh_models(&ui, &settings, &ui_models, lang_es);
                            ui.set_models_status(
                                tr.model_downloaded(&model.name, bytes as f64 / 1_048_576.0)
                                    .into(),
                            );
                        }
                        Err(error) => {
                            ui.set_models_status(tr.model_failed(&error.to_string()).into())
                        }
                    }
                });
            });
        });
    }
    {
        let ui_handle = ui.as_weak();
        let settings = settings.clone();
        let ui_models = ui_models.clone();
        let lang = lang.clone();
        ui.on_use_model(move |index| {
            let tr = Tr::new(lang.get());
            let Some(ui) = ui_handle.upgrade() else { return };
            let Some(model) = ui_models.lock().ok().and_then(|guard| guard.get(index as usize).cloned()) else { return };
            if !model.path.is_file() {
                ui.set_models_status(tr.model_missing(&model.path.display().to_string()).into());
                return;
            }
            if let Ok(mut guard) = settings.lock() {
                guard.active_model = model.path.to_string_lossy().to_string();
                guard.set_device_index(ui.get_semantic_device());
                guard.similarity = model.threshold;
                guard.upsert_model(InstalledModel {
                    name: model.name.clone(),
                    path: model.path.clone(),
                    capabilities: model.capabilities.clone(),
                    preset: model.preset.clone(),
                    input_size: model.input_size,
                    threshold: model.threshold,
                    source_url: model.url.clone(),
                });
                let _ = guard.save();
            }
            ui.set_semantic_enabled(true);
            ui.set_semantic_model(model.path.to_string_lossy().to_string().into());
            ui.set_semantic_preset(if model.preset == "clip" { 1 } else { 0 });
            ui.set_semantic_input_size(model.input_size as i32);
            ui.set_semantic_threshold(model.threshold);
            ui.set_semantic_threshold_label(format!("{:.2}", model.threshold).into());
            ui.set_models_status(tr.model_active(&model.name).into());
            refresh_models(&ui, &settings, &ui_models, lang.get() == Lang::Es);
        });
    }
    {
        let ui_handle = ui.as_weak();
        let settings = settings.clone();
        let ui_models = ui_models.clone();
        let lang = lang.clone();
        ui.on_remove_model(move |index| {
            let tr = Tr::new(lang.get());
            let Some(ui) = ui_handle.upgrade() else { return };
            let Some(model) = ui_models.lock().ok().and_then(|guard| guard.get(index as usize).cloned()) else { return };
            let _ = std::fs::remove_file(&model.path);
            let was_active = if let Ok(mut guard) = settings.lock() {
                let was_active = guard.active_model == model.path.to_string_lossy();
                guard.remove_model(&model.path);
                let _ = guard.save();
                was_active
            } else {
                false
            };
            if was_active {
                ui.set_semantic_enabled(false);
                ui.set_semantic_model("".into());
            }
            refresh_models(&ui, &settings, &ui_models, lang.get() == Lang::Es);
            ui.set_models_status(tr.model_removed(&model.name).into());
        });
    }
    {
        let ui_handle = ui.as_weak();
        let settings = settings.clone();
        let ui_models = ui_models.clone();
        let lang = lang.clone();
        ui.on_save_model_settings(move || {
            let Some(ui) = ui_handle.upgrade() else { return };
            save_model_settings(&ui, &settings);
            refresh_models(&ui, &settings, &ui_models, lang.get() == Lang::Es);
        });
    }
    {
        let ui_handle = ui.as_weak();
        let settings = settings.clone();
        let ui_models = ui_models.clone();
        let lang = lang.clone();
        ui.on_close_models(move || {
            let Some(ui) = ui_handle.upgrade() else { return };
            save_model_settings(&ui, &settings);
            refresh_models(&ui, &settings, &ui_models, lang.get() == Lang::Es);
            ui.set_models_open(false);
        });
    }

    // ---- folder list --------------------------------------------------------
    {
        let ui_handle = ui.as_weak();
        let folders = folders.clone();
        let folder_model = folder_model.clone();
        let lang = lang.clone();
        ui.on_remove_folder(move || {
            let Some(ui) = ui_handle.upgrade() else { return };
            let tr = Tr::new(lang.get());
            let idx = ui.get_folder_selected();
            if idx >= 0 && (idx as usize) < folder_model.row_count() {
                folders.borrow_mut().remove(idx as usize);
                folder_model.remove(idx as usize);
                ui.set_folder_selected(-1);
                ui.set_folders_label(tr.folders_label(folders.borrow().len()).into());
                ui.set_status(tr.status_folder_removed().into());
            }
        });
    }
    {
        let ui_handle = ui.as_weak();
        let folders = folders.clone();
        let folder_model = folder_model.clone();
        let lang = lang.clone();
        ui.on_clear_folders(move || {
            let Some(ui) = ui_handle.upgrade() else { return };
            let tr = Tr::new(lang.get());
            folders.borrow_mut().clear();
            folder_model.set_vec(Vec::new());
            ui.set_folder_selected(-1);
            ui.set_folders_label(tr.folders_label(0).into());
            ui.set_status(tr.status_folders_cleared().into());
        });
    }
    {
        let ui_handle = ui.as_weak();
        ui.on_select_folder(move |index| {
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_folder_selected(index);
            }
        });
    }

    // ---- threshold / tolerance labels --------------------------------------
    {
        let ui_handle = ui.as_weak();
        ui.on_threshold_changed(move |value| {
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_threshold_label(format!("{} %", value.round() as i32).into());
            }
        });
    }
    {
        let ui_handle = ui.as_weak();
        ui.on_tolerance_changed(move |value| {
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_tolerance_label(format!("{}", value.round() as i32).into());
            }
        });
    }

    // ---- selection + preview (decoded off the UI thread) --------------------
    {
        let ui_handle = ui.as_weak();
        let rows = rows.clone();
        let preview_cache = preview_cache.clone();
        let preview_gen = preview_gen.clone();
        ui.on_select_row(move |index| {
            let Some(ui) = ui_handle.upgrade() else { return };
            if index < 0 {
                return;
            }
            let Some(row) = rows.row_data(index as usize) else { return };
            ui.set_selected_row(index);
            ui.set_keep_confidence(row.keep_confidence.clone());
            ui.set_keep_reason(row.keep_reason.clone());

            // Latest request wins: rapid arrow-key navigation discards the
            // results of previous selections instead of queueing decodes.
            let generation = preview_gen.fetch_add(1, Ordering::SeqCst) + 1;
            let left_path = row.original_path.to_string();
            let right_path = row.duplicate_path.to_string();
            let ui_weak = ui_handle.clone();
            let cache = preview_cache.clone();
            let gen_handle = preview_gen.clone();

            std::thread::spawn(move || {
                let left = preview_data(&left_path, &cache);
                let right = preview_data(&right_path, &cache);
                let _ = slint::invoke_from_event_loop(move || {
                    if gen_handle.load(Ordering::SeqCst) != generation {
                        return;
                    }
                    let Some(ui) = ui_weak.upgrade() else { return };
                    if let Some((w, h, data, info)) = left {
                        ui.set_preview_left(rgba_to_image(w, h, &data));
                        ui.set_preview_left_info(info.into());
                    }
                    if let Some((w, h, data, info)) = right {
                        ui.set_preview_right(rgba_to_image(w, h, &data));
                        ui.set_preview_right_info(info.into());
                    }
                });
            });
        });
    }

    // ---- keyboard selection (Up/Down) ---------------------------------------
    {
        let ui_handle = ui.as_weak();
        let rows = rows.clone();
        ui.on_move_selection(move |delta| {
            let Some(ui) = ui_handle.upgrade() else { return };
            let count = rows.row_count();
            if count == 0 {
                return;
            }
            let current = ui.get_selected_row();
            let next = if current < 0 {
                0
            } else {
                (current + delta).clamp(0, count as i32 - 1)
            };
            if next != current {
                ui.invoke_select_row(next);
            }
        });
    }

    // ---- start / stop -------------------------------------------------------
    {
        let ui_handle = ui.as_weak();
        let folders = folders.clone();
        let cancel = cancel.clone();
        let prx = prx.clone();
        let drx = drx.clone();
        let rows = rows.clone();
        let lang = lang.clone();
        let cache = cache.clone();
        ui.on_start(move || {
            let Some(ui) = ui_handle.upgrade() else { return };
            let tr = Tr::new(lang.get());
            if ui.get_busy() {
                ui.set_status(tr.status_busy().into());
                return;
            }
            if folders.borrow().is_empty() {
                ui.set_status(tr.status_no_folders().into());
                ui.set_stage(tr.stage_no_folders().into());
                return;
            }

            while prx.borrow_mut().try_recv().is_ok() {}
            while drx.borrow_mut().try_recv().is_ok() {}
            cancel.store(false, Ordering::Relaxed);

            let mut cfg = ScanConfig::default();
            cfg.folders = folders.borrow().clone();
            cfg.recursive = ui.get_recursive();
            cfg.only_identical = ui.get_only_identical();
            cfg.compare_media_content = ui.get_compare_media();
            cfg.similarity_threshold = ui.get_threshold();
            cfg.pixel_tolerance = ui.get_tolerance().round().clamp(0.0, 255.0) as u8;
            cfg.keep_policy = KEEP_POLICIES[ui.get_keep_index().clamp(0, 7) as usize];
            cfg.metric = METRICS[ui.get_metric_index().clamp(0, 4) as usize];
            cfg.threads = THREAD_OPTIONS[ui.get_threads_index().clamp(0, 6) as usize];
            if ui.get_semantic_enabled() && !cfg!(feature = "embeddings") {
                ui.set_status(tr.no_embeddings_build().into());
                return;
            }
            if cfg!(feature = "embeddings") && ui.get_semantic_enabled() {
                let model = PathBuf::from(ui.get_semantic_model().to_string());
                if model.as_os_str().is_empty() {
                    ui.set_status(tr.model_select_first().into());
                    return;
                } else {
                    let clip = ui.get_semantic_preset() == 1;
                    cfg.embeddings = Some(EmbeddingConfig {
                        model_path: model,
                        input_size: ui.get_semantic_input_size().clamp(32, 1024) as u32,
                        batch_size: 32,
                        threshold: ui.get_semantic_threshold().clamp(0.0, 1.0),
                        use_cuda: ui.get_semantic_device() == 0,
                        mean: if clip {
                            [0.481_454_66, 0.457_827_5, 0.408_210_73]
                        } else {
                            [0.485, 0.456, 0.406]
                        },
                        std: if clip {
                            [0.268_629_54, 0.261_302_58, 0.275_777_11]
                        } else {
                            [0.229, 0.224, 0.225]
                        },
                    });
                }
            }
            cfg.extensions = default_image_extensions()
                .into_iter()
                .chain(default_video_extensions())
                .chain(default_audio_extensions())
                .collect();
            cfg.leave_one = true;
            cfg.ffmpeg = umdf_core::media::find_tool(&["ffmpeg", "ffmpeg.exe"]);
            if ui.get_compare_media() && cfg.ffmpeg.is_none() {
                ui.set_status(tr.status_ffmpeg_missing().into());
            }
            cfg.cancel = Some(cancel.clone());

            rows.set_vec(Vec::new());
            ui.set_selected_row(-1);
            ui.set_busy(true);
            ui.set_progress(0.0);
            ui.set_stage(tr.stage_starting().into());
            ui.set_status(tr.status_scanning().into());
            ui.set_scanned_label(tr.files_scanned(0).into());
            ui.set_found_label(tr.duplicates_found(0).into());
            ui.set_space_label(tr.reclaimable(&human_bytes(0)).into());
            ui.set_elapsed_label(tr.elapsed(0.0, 0, auto_threads).into());

            let ptx = ptx.clone();
            let dtx = dtx.clone();
            let cache = cache.clone();
            std::thread::spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    umdf_core::run_with_progress_cached(&cfg, Some(&ptx), Some(cache.as_ref()))
                }))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("scan thread panicked")));
                let _ = dtx.send(result.map_err(|e| e.to_string()));
            });
        });
    }
    {
        let ui_handle = ui.as_weak();
        let cancel = cancel.clone();
        let lang = lang.clone();
        ui.on_stop(move || {
            let Some(ui) = ui_handle.upgrade() else { return };
            let tr = Tr::new(lang.get());
            cancel.store(true, Ordering::Relaxed);
            ui.set_stage(tr.stage_stopping().into());
            ui.set_status(tr.status_stopping().into());
        });
    }

    let demo_select = std::env::args().any(|a| a == "--demo-select");

    // ---- event pump ---------------------------------------------------------
    // The Timer handle must outlive `ui.run()`; dropping it stops the timer.
    let pump = slint::Timer::default();
    {
        let ui_handle = ui.as_weak();
        let rows = rows.clone();
        let prx = prx.clone();
        let drx = drx.clone();
        let cancel = cancel.clone();
        let lang = lang.clone();
        let thumbs = thumbs.clone();
        let thumb_gen = thumb_gen.clone();
        let ui_stats = ui_stats.clone();
        pump.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(100),
            move || {
                let Some(ui) = ui_handle.upgrade() else { return };
                let tr = Tr::new(lang.get());

                loop {
                    let received = prx.borrow_mut().try_recv();
                    match received {
                        Ok(progress) => handle_progress(&ui, progress, &tr),
                        Err(_) => break,
                    }
                }

                let done = drx.borrow_mut().try_recv();
                if let Ok(result) = done {
                    ui.set_busy(false);
                    let stopped = cancel.load(Ordering::Relaxed);
                    match result {
                        Ok(report) => {
                            let stopped = stopped || report.stats.cancelled;
                            apply_report(&ui, &rows, &report, &tr, &thumbs, &thumb_gen, &ui_stats);
                            if demo_select {
                                ui.invoke_select_row(0);
                            }
                            ui.set_progress(100.0);
                            let message = if stopped {
                                ui.set_stage(tr.stage_stopped().into());
                                if report.stats.files_scanned == 0 {
                                    tr.status_stopped_zero()
                                } else {
                                    tr.status_stopped_partial(report.stats.files_scanned)
                                }
                            } else {
                                ui.set_stage(tr.stage_finished().into());
                                tr.status_finished(report.stats.groups, report.stats.duplicates_found)
                            };
                            ui.set_status(message.into());
                        }
                        Err(error) => {
                            ui.set_stage(tr.stage_error().into());
                            ui.set_status(tr.status_error(&error).into());
                        }
                    }
                }
            },
        );
    }

    // ---- per-row actions ----------------------------------------------------
    {
        let ui_handle = ui.as_weak();
        let rows = rows.clone();
        let lang = lang.clone();
        let ui_stats = ui_stats.clone();
        let preview_gen = preview_gen.clone();
        ui.on_trash_row(move |index| {
            let Some(ui) = ui_handle.upgrade() else { return };
            let tr = Tr::new(lang.get());
            if let Some((idx, path)) = row_path(&rows, index) {
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                let ok = trash::delete(&path).is_ok();
                if ok {
                    rows.remove(idx);
                    ui.set_status(tr.status_trashed(&path).into());
                    advance_after_removal(&ui, &rows, &tr, idx, &preview_gen);
                    update_counters(&ui, &rows, &tr, &ui_stats, size);
                } else {
                    ui.set_status(tr.status_trash_failed(&path).into());
                }
            }
        });
    }
    {
        let ui_handle = ui.as_weak();
        let rows = rows.clone();
        let lang = lang.clone();
        let ui_stats = ui_stats.clone();
        let preview_gen = preview_gen.clone();
        ui.on_delete_row(move |index| {
            let Some(ui) = ui_handle.upgrade() else { return };
            let tr = Tr::new(lang.get());
            if let Some((idx, path)) = row_path(&rows, index) {
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                let ok = std::fs::remove_file(&path).is_ok();
                if ok {
                    rows.remove(idx);
                    ui.set_status(tr.status_deleted(&path).into());
                    advance_after_removal(&ui, &rows, &tr, idx, &preview_gen);
                    update_counters(&ui, &rows, &tr, &ui_stats, size);
                } else {
                    ui.set_status(tr.status_delete_failed(&path).into());
                }
            }
        });
    }
    {
        let ui_handle = ui.as_weak();
        let rows = rows.clone();
        let lang = lang.clone();
        let ui_stats = ui_stats.clone();
        let preview_gen = preview_gen.clone();
        ui.on_move_row(move |index| {
            let Some(ui) = ui_handle.upgrade() else { return };
            let tr = Tr::new(lang.get());
            let dest = PathBuf::from(ui.get_move_dest().to_string());
            if dest.as_os_str().is_empty() {
                ui.set_status(tr.status_move_dest_missing().into());
                return;
            }
            if let Some((idx, path)) = row_path(&rows, index) {
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                let _ = std::fs::create_dir_all(&dest);
                let target = umdf_core::action::unique_destination(&dest, &path);
                if std::fs::rename(&path, &target).is_ok() {
                    rows.remove(idx);
                    ui.set_status(tr.status_moved(&target).into());
                    advance_after_removal(&ui, &rows, &tr, idx, &preview_gen);
                    update_counters(&ui, &rows, &tr, &ui_stats, size);
                } else {
                    ui.set_status(tr.status_move_failed(&path).into());
                }
            }
        });
    }

    // ---- bulk actions -------------------------------------------------------
    {
        let ui_handle = ui.as_weak();
        let rows = rows.clone();
        let lang = lang.clone();
        ui.on_apply_all(move |kind| {
            let Some(ui) = ui_handle.upgrade() else { return };
            let tr = Tr::new(lang.get());
            let count = rows.row_count();
            if count == 0 {
                ui.set_status(tr.status_nothing_to_apply().into());
                return;
            }
            let kind = kind.to_string();
            let verb = verb_for(&kind);
            let destination = if verb == Verb::Move {
                let dest = PathBuf::from(ui.get_move_dest().to_string());
                if dest.as_os_str().is_empty() {
                    ui.set_status(tr.status_move_dest_missing().into());
                    return;
                }
                Some(dest)
            } else {
                None
            };
            ui.set_confirm_kind(kind.as_str().into());
            ui.set_confirm_text(tr.confirm_bulk(verb, count, destination.as_deref()).into());
            ui.set_confirm_open(true);
            ui.set_status(tr.status_confirm_bulk().into());
        });
    }
    {
        let ui_handle = ui.as_weak();
        let rows = rows.clone();
        let lang = lang.clone();
        let ui_stats = ui_stats.clone();
        let preview_gen = preview_gen.clone();
        ui.on_confirm_apply(move || {
            let Some(ui) = ui_handle.upgrade() else { return };
            let tr = Tr::new(lang.get());
            let kind = ui.get_confirm_kind().to_string();
            let verb = verb_for(&kind);
            let dest = PathBuf::from(ui.get_move_dest().to_string());
            ui.set_confirm_open(false);

            let mut kept = Vec::new();
            let mut done = 0usize;
            let mut failed = 0usize;
            let mut bytes = 0u64;
            for i in 0..rows.row_count() {
                let Some(row) = rows.row_data(i) else { continue };
                let path = PathBuf::from(row.duplicate_path.as_str());
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                let ok = match verb {
                    Verb::Trash => trash::delete(&path).is_ok(),
                    Verb::Delete => std::fs::remove_file(&path).is_ok(),
                    Verb::Move => {
                        let _ = std::fs::create_dir_all(&dest);
                        let target = umdf_core::action::unique_destination(&dest, &path);
                        std::fs::rename(&path, &target).is_ok()
                    }
                };
                if ok {
                    done += 1;
                    bytes += size;
                } else {
                    failed += 1;
                    kept.push(row);
                }
            }
            rows.set_vec(kept);
            ui.set_selected_row(-1);
            update_counters(&ui, &rows, &tr, &ui_stats, bytes);
            if rows.row_count() == 0 {
                preview_gen.fetch_add(1, Ordering::SeqCst);
                ui.set_preview_left(slint::Image::default());
                ui.set_preview_right(slint::Image::default());
                ui.set_preview_left_info(tr.no_picture().into());
                ui.set_preview_right_info(tr.no_picture().into());
                ui.set_keep_confidence("".into());
                ui.set_keep_reason("".into());
            }
            ui.set_status(tr.bulk_done(done, verb, &human_bytes(bytes), failed).into());
        });
    }
    {
        let ui_handle = ui.as_weak();
        let lang = lang.clone();
        ui.on_confirm_cancel(move || {
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_confirm_open(false);
                ui.set_status(Tr::new(lang.get()).status_bulk_cancelled().into());
            }
        });
    }

    // ---- optional self-test hooks ------------------------------------------
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--demo-dark") {
        ui.set_dark_mode(true);
    }
    if let Some(pos) = args.iter().position(|a| a == "--demo") {
        if let Some(folder) = args.get(pos + 1) {
            add_folder(
                &ui,
                &folder_model,
                &folders,
                PathBuf::from(folder),
                &Tr::new(lang.get()),
            );
            let ui_weak = ui.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(300), move || {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.invoke_start();
                }
            });
        }
    }
    if args.iter().any(|a| a == "--demo-browser") {
        let ui_weak = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(600), move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.invoke_open_browser("home".into());
            }
        });
    }
    if let Some(pos) = args.iter().position(|a| a == "--demo-bulk") {
        if let Some(kind) = args.get(pos + 1) {
            let kind = kind.clone();
            let ui_weak = ui.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(4000), move || {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.invoke_apply_all(kind.as_str().into());
                    ui.invoke_confirm_apply();
                }
            });
        }
    }
    if let Some(pos) = args.iter().position(|a| a == "--demo-size") {
        if let Some(spec) = args.get(pos + 1) {
            let parts: Vec<f32> = spec.split('x').filter_map(|v| v.parse().ok()).collect();
            if parts.len() == 2 {
                ui.window()
                    .set_size(slint::LogicalSize::new(parts[0], parts[1]));
            }
        }
    }
    if let Some(pos) = args.iter().position(|a| a == "--semantic-model") {
        if let Some(model) = args.get(pos + 1) {
            ui.set_semantic_enabled(true);
            ui.set_semantic_model(model.as_str().into());
            ui.set_semantic_summary(
                format!(
                    "{} · CUDA · 224²",
                    Path::new(model).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
                )
                .into(),
            );
        }
    }
    if let Some(pos) = args.iter().position(|a| a == "--models-dir") {
        if let Some(dir) = args.get(pos + 1) {
            ui.set_models_dir(dir.as_str().into());
            save_model_settings(&ui, &settings);
            refresh_models(&ui, &settings, &ui_models, lang.get() == Lang::Es);
        }
    }
    if args.iter().any(|a| a == "--demo-models") {
        refresh_models(&ui, &settings, &ui_models, lang.get() == Lang::Es);
        ui.set_models_open(true);
    }
    if let Some(pos) = args.iter().position(|a| a == "--demo-use-model") {
        if let Some(index) = args.get(pos + 1).and_then(|v| v.parse::<i32>().ok()) {
            ui.set_models_open(true);
            ui.invoke_use_model(index);
            ui.set_models_open(false);
        }
    }
    if args.iter().any(|a| a == "--demo-help") {
        ui.set_help_open(true);
    }
    if args.iter().any(|a| a == "--demo-max") {
        ui.window().set_maximized(true);
    }
    if let Some(pos) = args.iter().position(|a| a == "--demo-delete") {
        if let Some(n) = args.get(pos + 1).and_then(|v| v.parse::<usize>().ok()) {
            let ui_weak = ui.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(5000), move || {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.invoke_select_row(0);
                    for _ in 0..n {
                        let selected = ui.get_selected_row();
                        if selected < 0 {
                            break;
                        }
                        ui.invoke_delete_row(selected);
                    }
                }
            });
        }
    }
    if let Some(pos) = args.iter().position(|a| a == "--demo-stop-ms") {
        if let Some(ms) = args.get(pos + 1).and_then(|v| v.parse::<u64>().ok()) {
            let cancel = cancel.clone();
            slint::Timer::single_shot(std::time::Duration::from_millis(ms), move || {
                cancel.store(true, Ordering::Relaxed);
            });
        }
    }
    if let Some(pos) = args.iter().position(|a| a == "--demo-restart-ms") {
        if let Some(ms) = args.get(pos + 1).and_then(|v| v.parse::<u64>().ok()) {
            let ui_weak = ui.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(ms), move || {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.invoke_start();
                }
            });
        }
    }
    if let Some(pos) = args.iter().position(|a| a == "--demo-lang") {
        if let Some(code) = args.get(pos + 1) {
            let index = if code.eq_ignore_ascii_case("es") { 1 } else { 0 };
            ui.set_lang_index(index);
            ui.invoke_set_lang(index);
        }
    }

    ui.run()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn verb_for(kind: &str) -> Verb {
    match kind {
        "delete" => Verb::Delete,
        "move" => Verb::Move,
        _ => Verb::Trash,
    }
}

fn update_cache_label(ui: &AppWindow, cache: &CacheDb) {
    let stats = cache.stats();
    ui.set_models_db(
        format!(
            "config.toml: {} · cache.db: {} ({} fingerprints · {} embeddings · {})",
            Settings::path().display(),
            cache.path().display(),
            stats.fingerprints,
            stats.embeddings,
            human_bytes(stats.bytes)
        )
        .into(),
    );
}

fn save_model_settings(ui: &AppWindow, settings: &Arc<Mutex<Settings>>) {
    let models_dir = PathBuf::from(ui.get_models_dir().to_string());
    let device = ui.get_semantic_device();
    let similarity = ui.get_semantic_threshold();
    if let Ok(mut guard) = settings.lock() {
        // The slider fires on every step; only touch the disk when something
        // actually changed.
        let changed = guard.models_dir != models_dir
            || guard.device_index() != device
            || (guard.similarity - similarity).abs() > 0.0001;
        if changed {
            guard.models_dir = models_dir;
            guard.set_device_index(device);
            guard.similarity = similarity;
            if let Err(error) = guard.save() {
                eprintln!("UMDF: could not save config.toml: {error}");
            }
        }
    }
    ui.set_semantic_threshold_label(format!("{similarity:.2}").into());
}

fn refresh_models(
    ui: &AppWindow,
    settings: &Arc<Mutex<Settings>>,
    ui_models: &Arc<Mutex<Vec<UiModel>>>,
    es: bool,
) {
    let dir = PathBuf::from(ui.get_models_dir().to_string());
    let (active, installed) = if let Ok(guard) = settings.lock() {
        (guard.active_model.clone(), guard.models.clone())
    } else {
        (String::new(), Vec::new())
    };

    let mut rows: Vec<ModelRow> = Vec::new();
    let mut entries: Vec<UiModel> = Vec::new();

    for catalog in models::CATALOG {
        let path = dir.join(format!("{}.onnx", catalog.id));
        let is_active = !active.is_empty() && Path::new(&active) == path;
        let state = if is_active {
            2
        } else if path.is_file() {
            1
        } else {
            0
        };
        rows.push(ModelRow {
            name: catalog.name(es).into(),
            details: format!(
                "{} · ~{} MB",
                catalog.capabilities(es),
                catalog.size_mb
            )
            .into(),
            path: path.to_string_lossy().to_string().into(),
            state,
        });
        entries.push(UiModel {
            name: catalog.name(false).to_string(),
            path,
            url: catalog.url.to_string(),
            capabilities: catalog.capabilities(false).to_string(),
            preset: catalog.preset.to_string(),
            input_size: catalog.input_size,
            threshold: catalog.threshold,
        });
    }

    for model in installed {
        if entries.iter().any(|entry| entry.path == model.path) {
            continue;
        }
        let is_active = !active.is_empty() && Path::new(&active) == model.path;
        rows.push(ModelRow {
            name: model.name.clone().into(),
            details: model.capabilities.clone().into(),
            path: model.path.to_string_lossy().to_string().into(),
            state: if is_active { 2 } else { 1 },
        });
        entries.push(UiModel {
            name: model.name,
            path: model.path,
            url: model.source_url,
            capabilities: model.capabilities,
            preset: model.preset,
            input_size: model.input_size,
            threshold: model.threshold,
        });
    }

    ui.set_model_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
    if let Ok(mut guard) = ui_models.lock() {
        *guard = entries;
    }

    let summary = if active.is_empty() {
        if es {
            "Ningún modelo seleccionado".to_string()
        } else {
            "No model selected".to_string()
        }
    } else {
        let name = Path::new(&active)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| active.clone());
        let device = if ui.get_semantic_device() == 0 {
            "CUDA"
        } else {
            "CPU"
        };
        format!("{name} · {device} · {}²", ui.get_semantic_input_size())
    };
    ui.set_semantic_summary(summary.into());
}

/// Read the model's input shape when the embeddings feature is enabled and
/// suggest the matching preprocessing.
fn detect_model(model: &UiModel) -> (u32, String) {
    #[cfg(feature = "embeddings")]
    {
        if let Ok(info) = umdf_core::embeddings::inspect_model(&model.path) {
            let path = model.path.to_string_lossy().to_ascii_lowercase();
            let size = info.input_size.unwrap_or(model.input_size);
            let preset = if path.contains("clip") || path.contains("siglip") || size >= 336 {
                "clip".to_string()
            } else {
                "imagenet".to_string()
            };
            return (size, preset);
        }
    }
    (model.input_size, model.preset.clone())
}

fn handle_progress(ui: &AppWindow, progress: Progress, tr: &Tr) {
    match progress {
        Progress::Discovered { total } => {
            ui.set_stage(tr.stage_loading_total(total).into());
            ui.set_progress(1.0);
        }
        Progress::Loaded {
            index,
            total,
            path,
        } => {
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| path.to_string_lossy().to_string());
            ui.set_stage(tr.stage_loading(&name).into());
            let pct = if total > 0 {
                40.0 * index as f32 / total as f32
            } else {
                0.0
            };
            ui.set_progress(pct);
            ui.set_scanned_label(tr.files_scanned_progress(index, total).into());
        }
        Progress::Embedding { done, total } => {
            ui.set_stage(tr.stage_embedding(done, total).into());
            let pct = if total > 0 {
                40.0 + 10.0 * done as f32 / total as f32
            } else {
                50.0
            };
            ui.set_progress(pct);
        }
        Progress::Hashing { done, total } => {
            ui.set_stage(tr.stage_hashing(done, total).into());
            let pct = if total > 0 {
                50.0 + 5.0 * done as f32 / total as f32
            } else {
                55.0
            };
            ui.set_progress(pct);
        }
        Progress::Comparing { done, total } => {
            ui.set_stage(tr.stage_comparing(done, total).into());
            let pct = if total > 0 {
                55.0 + 43.0 * done as f32 / total as f32
            } else {
                98.0
            };
            ui.set_progress(pct);
        }
        Progress::Grouped { groups, duplicates } => {
            ui.set_stage(tr.stage_grouping().into());
            ui.set_progress(98.0);
            ui.set_found_label(tr.duplicates_found_groups(duplicates, groups).into());
        }
        Progress::Finished => {
            ui.set_progress(100.0);
            ui.set_stage(tr.stage_finished().into());
        }
    }
}

fn apply_report(
    ui: &AppWindow,
    rows: &Rc<VecModel<RowItem>>,
    report: &ScanReport,
    tr: &Tr,
    thumb_cache: &Arc<Mutex<HashMap<String, Option<ThumbData>>>>,
    thumb_gen: &Arc<AtomicU64>,
    ui_stats: &RefCell<UiStats>,
) {
    let mut model: Vec<RowItem> = Vec::new();
    let mut jobs: Vec<(usize, String, String)> = Vec::new();

    for group in &report.groups {
        let keep = &report.items[group.keep];
        for id in &group.remove {
            let item = &report.items[*id];
            let idx = model.len();
            let keep_path = keep.path.to_string_lossy().to_string();
            let dup_path = item.path.to_string_lossy().to_string();
            if idx < MAX_THUMBNAIL_ROWS {
                jobs.push((idx, keep_path.clone(), dup_path.clone()));
            }
            let reasons = tr.reason_labels(&group.keep_reasons);
            model.push(RowItem {
                group: group.number.to_string().into(),
                original: keep.file_name().into(),
                duplicate: item.file_name().into(),
                similarity: format!("{:.1}%", group.max_similarity).into(),
                size: human_bytes(item.size).into(),
                original_path: keep_path.into(),
                duplicate_path: dup_path.into(),
                keep_confidence: tr.original_likely(group.keep_confidence).into(),
                keep_reason: reasons.into(),
                keep_pct: group.keep_confidence,
                keep_codes: group.keep_reasons.join(",").into(),
                original_thumb: slint::Image::default(),
                duplicate_thumb: slint::Image::default(),
            });
        }
    }
    rows.set_vec(model);

    *ui_stats.borrow_mut() = UiStats {
        scanned: report.stats.files_scanned,
        found: report.stats.duplicates_found,
        groups: report.stats.groups,
        reclaimable: report.stats.reclaimable_bytes,
        elapsed_ms: report.stats.elapsed_ms,
        threads: report.stats.threads,
    };

    // Thumbnails are generated in the background so the list appears instantly.
    let generation = thumb_gen.fetch_add(1, Ordering::SeqCst) + 1;
    if !jobs.is_empty() {
        spawn_thumbnails(ui.as_weak(), jobs, thumb_cache.clone(), thumb_gen.clone(), generation);
    }

    ui.set_scanned_label(tr.files_scanned(report.stats.files_scanned).into());
    ui.set_found_label(tr.duplicates_found(report.stats.duplicates_found).into());
    ui.set_space_label(tr.reclaimable(&human_bytes(report.stats.reclaimable_bytes)).into());
    ui.set_elapsed_label(
        tr.elapsed(
            report.stats.elapsed_ms as f64 / 1000.0,
            report.stats.groups,
            report.stats.threads,
        )
        .into(),
    );

    if ui.get_selected_row() >= 0 {
        if let Some(row) = rows.row_data(ui.get_selected_row() as usize) {
            ui.set_keep_confidence(row.keep_confidence.clone());
            ui.set_keep_reason(row.keep_reason.clone());
        }
    }
}

fn spawn_thumbnails(
    ui_weak: slint::Weak<AppWindow>,
    jobs: Vec<(usize, String, String)>,
    cache: Arc<Mutex<HashMap<String, Option<ThumbData>>>>,
    generation: Arc<AtomicU64>,
    my_generation: u64,
) {
    std::thread::spawn(move || {
        use rayon::prelude::*;
        for chunk in jobs.chunks(32) {
            if generation.load(Ordering::SeqCst) != my_generation {
                return;
            }
            let computed: Vec<(usize, Option<ThumbData>, Option<ThumbData>, String, String)> = chunk
                .par_iter()
                .map(|(idx, left, right)| {
                    (
                        *idx,
                        thumb_data(left, &cache),
                        thumb_data(right, &cache),
                        left.clone(),
                        right.clone(),
                    )
                })
                .collect();

            for (idx, left, right, left_path, right_path) in computed {
                let ui_weak = ui_weak.clone();
                let gen_handle = generation.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if gen_handle.load(Ordering::SeqCst) != my_generation {
                        return;
                    }
                    let Some(ui) = ui_weak.upgrade() else { return };
                    let model = ui.get_rows();
                    let Some(mut row) = model.row_data(idx) else { return };
                    if row.original_path.as_str() != left_path
                        || row.duplicate_path.as_str() != right_path
                    {
                        return;
                    }
                    if let Some((w, h, data)) = left {
                        row.original_thumb = rgba_to_image(w, h, &data);
                    }
                    if let Some((w, h, data)) = right {
                        row.duplicate_thumb = rgba_to_image(w, h, &data);
                    }
                    model.set_row_data(idx, row);
                });
            }
        }
    });
}

fn open_browser(
    ui: &AppWindow,
    model: &VecModel<BrowserEntry>,
    current: &RefCell<PathBuf>,
    path: PathBuf,
    tr: &Tr,
) {
    let canonical = path.canonicalize().unwrap_or(path);
    let entries = read_directories(&canonical, ui.get_show_hidden());
    let count = entries.len();
    model.set_vec(entries);
    *current.borrow_mut() = canonical.clone();
    ui.set_browser_path(canonical.to_string_lossy().to_string().into());
    let status = if ui.get_browser_mode() == "destination" {
        tr.browser_destination()
    } else {
        tr.browser_folders(count)
    };
    ui.set_browser_status(status.into());
    ui.set_browser_open(true);
}

fn read_directories(path: &Path, show_hidden: bool) -> Vec<BrowserEntry> {
    umdf_core::walk::list_directories(path)
        .into_iter()
        .filter(|(name, _)| show_hidden || !name.starts_with('.'))
        .map(|(name, path)| BrowserEntry {
            name: name.into(),
            path: path.to_string_lossy().to_string().into(),
        })
        .collect()
}

fn add_folder(
    ui: &AppWindow,
    model: &VecModel<FolderItem>,
    folders: &RefCell<Vec<PathBuf>>,
    path: PathBuf,
    tr: &Tr,
) {
    if !path.is_dir() {
        ui.set_status(tr.status_not_a_folder(&path).into());
        return;
    }
    if folders.borrow().iter().any(|p| p == &path) {
        ui.set_status(tr.status_folder_exists().into());
        return;
    }
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    folders.borrow_mut().push(path.clone());
    model.push(FolderItem {
        name: name.into(),
        path: path.to_string_lossy().to_string().into(),
    });
    ui.set_folders_label(tr.folders_label(folders.borrow().len()).into());
    ui.set_status(tr.status_folder_added(&path).into());
}

fn quick_location(kind: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    match kind {
        "home" => home,
        "pictures" => home.map(|h| h.join("Pictures")),
        "documents" => home.map(|h| h.join("Documents")),
        "root" => {
            #[cfg(windows)]
            {
                Some(PathBuf::from("C:\\"))
            }
            #[cfg(not(windows))]
            {
                Some(PathBuf::from("/"))
            }
        }
        _ => None,
    }
}

fn current_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}


/// Keep the status-bar counters honest after rows leave the list.
fn update_counters(
    ui: &AppWindow,
    rows: &VecModel<RowItem>,
    tr: &Tr,
    stats: &RefCell<UiStats>,
    removed_bytes: u64,
) {
    let mut state = stats.borrow_mut();
    state.found = rows.row_count();
    state.reclaimable = state.reclaimable.saturating_sub(removed_bytes);
    ui.set_found_label(tr.duplicates_found(state.found).into());
    ui.set_space_label(tr.reclaimable(&human_bytes(state.reclaimable)).into());
}

/// After deleting/trashing/moving the row at `removed_index`, select the row
/// that took its place (or the last one) and load its preview, so the user can
/// keep pressing Delete while visually comparing.
fn advance_after_removal(
    ui: &AppWindow,
    rows: &VecModel<RowItem>,
    tr: &Tr,
    removed_index: usize,
    preview_gen: &AtomicU64,
) {
    let count = rows.row_count();
    if count == 0 {
        // Invalidate any in-flight preview decode so it cannot repaint the
        // cleared panes.
        preview_gen.fetch_add(1, Ordering::SeqCst);
        ui.set_selected_row(-1);
        ui.set_preview_left(slint::Image::default());
        ui.set_preview_right(slint::Image::default());
        ui.set_preview_left_info(tr.no_picture().into());
        ui.set_preview_right_info(tr.no_picture().into());
        ui.set_keep_confidence("".into());
        ui.set_keep_reason("".into());
    } else {
        let next = removed_index.min(count - 1);
        ui.invoke_select_row(next as i32);
    }
}

fn row_path(rows: &VecModel<RowItem>, index: i32) -> Option<(usize, PathBuf)> {
    if index < 0 {
        return None;
    }
    let idx = index as usize;
    let row = rows.row_data(idx)?;
    Some((idx, PathBuf::from(row.duplicate_path.as_str())))
}

fn row_original_info(row: &RowItem) -> slint::SharedString {
    describe(&row.original_path)
}

fn row_duplicate_info(row: &RowItem) -> slint::SharedString {
    describe(&row.duplicate_path)
}

fn describe(path: &str) -> slint::SharedString {
    describe_string(path).into()
}

fn describe_string(path: &str) -> String {
    let p = Path::new(path);
    let name = p
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string());
    let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let dims = image::image_dimensions(p)
        .map(|(w, h)| format!("{w} x {h}"))
        .unwrap_or_else(|_| "-".to_string());
    format!("{name}\n{dims} · {}", human_bytes(size))
}

fn rgba_to_image(w: u32, h: u32, data: &[u8]) -> slint::Image {
    let mut buffer = SharedPixelBuffer::<slint::Rgba8Pixel>::new(w.max(1), h.max(1));
    let len = buffer.make_mut_bytes().len();
    let copy = data.len().min(len);
    buffer.make_mut_bytes()[..copy].copy_from_slice(&data[..copy]);
    slint::Image::from_rgba8(buffer)
}

fn thumb_data(
    path: &str,
    cache: &Mutex<HashMap<String, Option<ThumbData>>>,
) -> Option<ThumbData> {
    if let Some(value) = cache.lock().ok()?.get(path) {
        return value.clone();
    }
    let value = (|| {
        let img = image::open(path).ok()?;
        let thumb = img.thumbnail(52, 52).to_rgba8();
        let (w, h) = thumb.dimensions();
        Some((w, h, thumb.into_raw()))
    })();
    if let Ok(mut guard) = cache.lock() {
        if guard.len() > 512 {
            guard.clear();
        }
        guard.insert(path.to_string(), value.clone());
    }
    value
}

fn preview_data(
    path: &str,
    cache: &Mutex<HashMap<String, Option<PreviewData>>>,
) -> Option<PreviewData> {
    if let Some(value) = cache.lock().ok()?.get(path) {
        return value.clone();
    }
    let value = (|| {
        let img = image::open(path).ok()?;
        let thumb = img.thumbnail(480, 360).to_rgba8();
        let (w, h) = thumb.dimensions();
        Some((w, h, thumb.into_raw(), describe_string(path)))
    })();
    if let Ok(mut guard) = cache.lock() {
        if guard.len() > 24 {
            guard.clear();
        }
        guard.insert(path.to_string(), value.clone());
    }
    value
}
