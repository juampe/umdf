//! Tiny, dependency-free i18n for the UI (English / Spanish).
//!
//! The Slint side receives a `Strings` struct with every static label; dynamic
//! messages (stage, status, counters) are built here with `format!`.

use std::path::Path;

use crate::{HelpLine, Strings};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Es,
}

impl Lang {
    pub fn from_index(index: i32) -> Self {
        if index == 1 {
            Lang::Es
        } else {
            Lang::En
        }
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

impl Tr {
    pub fn new(lang: Lang) -> Self {
        Self { lang }
    }

    fn es(&self) -> bool {
        self.lang == Lang::Es
    }

    pub fn strings(&self) -> Strings {
        if self.es() {
            Strings {
                title: "Universal Media Duplicate Finder".into(),
                add_folder: "Añadir carpeta…".into(),
                remove: "Quitar".into(),
                clear: "Limpiar".into(),
                options: "Opciones".into(),
                start: "Buscar".into(),
                scanning: "Buscando…".into(),
                stop: "Parar".into(),
                dark: "Oscuro".into(),
                light: "Claro".into(),
                language: "Idioma".into(),
                help: "Ayuda".into(),
                help_title: "Ayuda".into(),
                close: "Cerrar".into(),
                locations: "Ubicaciones".into(),
                pictures: "Imágenes".into(),
                documents: "Documentos".into(),
                home: "Inicio".into(),
                computer: "Equipo".into(),
                folders_none: "Carpetas a escanear (ninguna)".into(),
                remove_selected: "Quitar seleccionada".into(),
                clear_all: "Quitar todas".into(),
                opt_scan_subfolders: "Incluir subcarpetas".into(),
                opt_only_identical: "Solo 100 % idénticos".into(),
                opt_compare_media: "Comparar contenido multimedia".into(),
                opt_similarity: "Similitud".into(),
                opt_keep: "Conservar".into(),
                opt_metric: "Métrica".into(),
                opt_threads: "Hilos".into(),
                opt_semantic: "Semántica (GPU)".into(),
                semantic_model: "Ruta del modelo .onnx".into(),
                opt_tolerance: "Tolerancia de gris".into(),
                trash_all: "A la papelera".into(),
                move_all: "Mover todos".into(),
                delete_all: "Borrar todos".into(),
                keeps_best: "conserva el original de cada grupo".into(),
                cancel: "Cancelar".into(),
                confirm: "Confirmar".into(),
                match_list: "Lista de coincidencias".into(),
                col_group: "Grupo".into(),
                col_original: "Original".into(),
                col_duplicate: "Duplicado".into(),
                col_similarity: "Similitud".into(),
                col_size: "Tamaño".into(),
                empty_list: "La lista de coincidencias está vacía.".into(),
                preview: "Vista previa".into(),
                original: "Original".into(),
                duplicate: "Duplicado".into(),
                move_destination: "Carpeta de destino".into(),
                move_placeholder: "/ruta/para/duplicados".into(),
                browse: "Examinar…".into(),
                trash: "Papelera".into(),
                r#move: "Mover".into(),
                delete: "Borrar".into(),
                select_folder: "Selecciona una carpeta".into(),
                up: "Subir".into(),
                go: "Ir".into(),
                select_this_folder: "Usar esta carpeta".into(),
                show_hidden: "Mostrar ocultas".into(),
            }
        } else {
            Strings {
                title: "Universal Media Duplicate Finder".into(),
                add_folder: "Add folder…".into(),
                remove: "Remove".into(),
                clear: "Clear".into(),
                options: "Options".into(),
                start: "Start".into(),
                scanning: "Scanning…".into(),
                stop: "Stop".into(),
                dark: "Dark".into(),
                light: "Light".into(),
                language: "Language".into(),
                help: "Help".into(),
                help_title: "Help".into(),
                close: "Close".into(),
                locations: "Locations".into(),
                pictures: "Pictures".into(),
                documents: "Documents".into(),
                home: "Home".into(),
                computer: "Computer".into(),
                folders_none: "Folders to scan (none)".into(),
                remove_selected: "Remove selected".into(),
                clear_all: "Clear all".into(),
                opt_scan_subfolders: "Scan subfolders".into(),
                opt_only_identical: "Only 100% identical".into(),
                opt_compare_media: "Compare media content".into(),
                opt_similarity: "Similarity".into(),
                opt_keep: "Keep".into(),
                opt_metric: "Metric".into(),
                opt_threads: "Threads".into(),
                opt_semantic: "Semantic (GPU)".into(),
                semantic_model: "Path to the .onnx model".into(),
                opt_tolerance: "Pixel tolerance".into(),
                trash_all: "Trash all duplicates".into(),
                move_all: "Move all".into(),
                delete_all: "Delete all".into(),
                keeps_best: "keeps the original of each group".into(),
                cancel: "Cancel".into(),
                confirm: "Confirm".into(),
                match_list: "Match list".into(),
                col_group: "Group".into(),
                col_original: "Original".into(),
                col_duplicate: "Duplicate".into(),
                col_similarity: "Similarity".into(),
                col_size: "Size".into(),
                empty_list: "The match list is empty.".into(),
                preview: "Preview".into(),
                original: "Original".into(),
                duplicate: "Duplicate".into(),
                move_destination: "Move destination".into(),
                move_placeholder: "/path/for/duplicates".into(),
                browse: "Browse…".into(),
                trash: "Trash".into(),
                r#move: "Move".into(),
                delete: "Delete".into(),
                select_folder: "Select a folder".into(),
                up: "Up".into(),
                go: "Go".into(),
                select_this_folder: "Select this folder".into(),
                show_hidden: "Show hidden".into(),
            }
        }
    }

    pub fn folders_label(&self, n: usize) -> String {
        if n == 0 {
            self.strings().folders_none.to_string()
        } else if self.es() {
            format!("Carpetas a escanear ({n})")
        } else {
            format!("Folders to scan ({n})")
        }
    }

    // ---- stage -----------------------------------------------------------
    pub fn stage_ready(&self) -> String {
        if self.es() { "Listo".into() } else { "Ready".into() }
    }
    pub fn stage_starting(&self) -> String {
        if self.es() { "Iniciando…".into() } else { "Starting…".into() }
    }
    pub fn stage_loading_total(&self, total: usize) -> String {
        if self.es() {
            format!("Etapa 1 de 2 — Cargando (0 / {total})")
        } else {
            format!("Stage 1 of 2 — Loading (0 / {total})")
        }
    }
    pub fn stage_loading(&self, name: &str) -> String {
        if self.es() {
            format!("Etapa 1 de 2 — Cargando: {name}")
        } else {
            format!("Stage 1 of 2 — Loading: {name}")
        }
    }
    pub fn stage_embedding(&self, done: usize, total: usize) -> String {
        if self.es() {
            format!("Embebiendo {done} / {total}")
        } else {
            format!("Embedding {done} / {total}")
        }
    }
    pub fn stage_hashing(&self, done: usize, total: usize) -> String {
        if self.es() {
            format!("Etapa 1 de 2 — Huellas {done} / {total}")
        } else {
            format!("Stage 1 of 2 — Hashing {done} / {total}")
        }
    }
    pub fn stage_comparing(&self, done: usize, total: usize) -> String {
        if self.es() {
            format!("Etapa 2 de 2 — Comparando {done} / {total}")
        } else {
            format!("Stage 2 of 2 — Comparing {done} / {total}")
        }
    }
    pub fn stage_grouping(&self) -> String {
        if self.es() {
            "Creando grupos…".into()
        } else {
            "Creating matching groups…".into()
        }
    }
    pub fn stage_finished(&self) -> String {
        if self.es() { "Terminado".into() } else { "Finished".into() }
    }
    pub fn stage_stopping(&self) -> String {
        if self.es() {
            "Deteniendo…".into()
        } else {
            "Stopping…".into()
        }
    }
    pub fn stage_stopped(&self) -> String {
        if self.es() { "Detenido".into() } else { "Stopped".into() }
    }
    pub fn stage_error(&self) -> String {
        if self.es() { "Error".into() } else { "Error".into() }
    }
    pub fn stage_no_folders(&self) -> String {
        if self.es() {
            "Ninguna carpeta seleccionada".into()
        } else {
            "No folders selected".into()
        }
    }

    // ---- status ----------------------------------------------------------
    pub fn status_ready(&self) -> String {
        if self.es() {
            "Añade una carpeta para empezar.".into()
        } else {
            "Add a folder to begin.".into()
        }
    }
    pub fn status_scanning(&self) -> String {
        if self.es() {
            "Escaneando — la interfaz sigue respondiendo.".into()
        } else {
            "Scanning — the interface stays responsive.".into()
        }
    }
    pub fn status_busy(&self) -> String {
        if self.es() {
            "Ya hay un escaneo en curso — pulsa Parar primero.".into()
        } else {
            "A scan is already running — press Stop first.".into()
        }
    }
    pub fn status_no_folders(&self) -> String {
        if self.es() {
            "Añade al menos una carpeta con «Añadir carpeta…».".into()
        } else {
            "Add at least one folder with “Add folder…”.".into()
        }
    }
    pub fn status_stopping(&self) -> String {
        if self.es() {
            "Deteniendo…".into()
        } else {
            "Stopping…".into()
        }
    }
    pub fn status_ffmpeg_missing(&self) -> String {
        if self.es() {
            "No se encontró ffmpeg/ffprobe: comparación de contenido multimedia desactivada."
                .into()
        } else {
            "ffmpeg/ffprobe not found — video/audio content comparison disabled.".into()
        }
    }
    pub fn status_folder_added(&self, path: &Path) -> String {
        if self.es() {
            format!("Añadida: {}", path.display())
        } else {
            format!("Added: {}", path.display())
        }
    }
    pub fn status_folder_exists(&self) -> String {
        if self.es() {
            "Esa carpeta ya está en la lista.".into()
        } else {
            "That folder is already in the list.".into()
        }
    }
    pub fn status_not_a_folder(&self, path: &Path) -> String {
        if self.es() {
            format!("No es una carpeta: {}", path.display())
        } else {
            format!("Not a folder: {}", path.display())
        }
    }
    pub fn status_folder_removed(&self) -> String {
        if self.es() {
            "Carpeta quitada.".into()
        } else {
            "Folder removed.".into()
        }
    }
    pub fn status_folders_cleared(&self) -> String {
        if self.es() {
            "Lista de carpetas vaciada.".into()
        } else {
            "Folder list cleared.".into()
        }
    }
    pub fn status_move_dest_missing(&self) -> String {
        if self.es() {
            "Configura primero la carpeta de destino.".into()
        } else {
            "Set a move destination first.".into()
        }
    }
    pub fn status_nothing_to_apply(&self) -> String {
        if self.es() {
            "No hay nada que aplicar.".into()
        } else {
            "There is nothing to apply.".into()
        }
    }
    pub fn status_confirm_bulk(&self) -> String {
        if self.es() {
            "Confirma la acción masiva en la barra naranja.".into()
        } else {
            "Confirm the bulk action in the yellow bar.".into()
        }
    }
    pub fn status_bulk_cancelled(&self) -> String {
        if self.es() {
            "Acción masiva cancelada.".into()
        } else {
            "Bulk action cancelled.".into()
        }
    }
    pub fn status_trashed(&self, path: &Path) -> String {
        if self.es() {
            format!("A la papelera: {}", path.display())
        } else {
            format!("Moved to trash: {}", path.display())
        }
    }
    pub fn status_trash_failed(&self, path: &Path) -> String {
        if self.es() {
            format!("No se pudo enviar a la papelera: {}", path.display())
        } else {
            format!("Could not trash: {}", path.display())
        }
    }
    pub fn status_deleted(&self, path: &Path) -> String {
        if self.es() {
            format!("Borrado: {}", path.display())
        } else {
            format!("Deleted: {}", path.display())
        }
    }
    pub fn status_delete_failed(&self, path: &Path) -> String {
        if self.es() {
            format!("No se pudo borrar: {}", path.display())
        } else {
            format!("Could not delete: {}", path.display())
        }
    }
    pub fn status_moved(&self, path: &Path) -> String {
        if self.es() {
            format!("Movido a: {}", path.display())
        } else {
            format!("Moved to: {}", path.display())
        }
    }
    pub fn status_move_failed(&self, path: &Path) -> String {
        if self.es() {
            format!("No se pudo mover: {}", path.display())
        } else {
            format!("Could not move: {}", path.display())
        }
    }
    pub fn status_error(&self, error: &str) -> String {
        if self.es() {
            format!("Error: {error}")
        } else {
            format!("Error: {error}")
        }
    }

    pub fn confirm_bulk(&self, verb: Verb, count: usize, destination: Option<&Path>) -> String {
        let dest = destination
            .map(|p| format!(" → {}", p.display()))
            .unwrap_or_default();
        match (self.es(), verb) {
            (false, Verb::Trash) => format!(
                "This will move {count} duplicate file(s){dest} to the trash, keeping the most likely original of every group."
            ),
            (false, Verb::Move) => format!(
                "This will move {count} duplicate file(s){dest}, keeping the most likely original of every group."
            ),
            (false, Verb::Delete) => format!(
                "This will permanently delete {count} duplicate file(s), keeping the most likely original of every group. This cannot be undone."
            ),
            (true, Verb::Trash) => format!(
                "Se enviarán {count} archivo(s) duplicado(s){dest} a la papelera, conservando el original más probable de cada grupo."
            ),
            (true, Verb::Move) => format!(
                "Se moverán {count} archivo(s) duplicado(s){dest}, conservando el original más probable de cada grupo."
            ),
            (true, Verb::Delete) => format!(
                "Se borrarán definitivamente {count} archivo(s) duplicado(s), conservando el original más probable de cada grupo. Esta acción no se puede deshacer."
            ),
        }
    }

    pub fn bulk_done(&self, done: usize, verb: Verb, bytes: &str, failed: usize) -> String {
        match (self.es(), verb) {
            (false, Verb::Trash) => {
                format!("Bulk action finished: {done} file(s) trashed ({bytes}), {failed} failed.")
            }
            (false, Verb::Move) => {
                format!("Bulk action finished: {done} file(s) moved ({bytes}), {failed} failed.")
            }
            (false, Verb::Delete) => {
                format!("Bulk action finished: {done} file(s) deleted ({bytes}), {failed} failed.")
            }
            (true, Verb::Trash) => format!(
                "Acción masiva terminada: {done} archivo(s) a la papelera ({bytes}), {failed} con error."
            ),
            (true, Verb::Move) => format!(
                "Acción masiva terminada: {done} archivo(s) movidos ({bytes}), {failed} con error."
            ),
            (true, Verb::Delete) => format!(
                "Acción masiva terminada: {done} archivo(s) borrados ({bytes}), {failed} con error."
            ),
        }
    }

    // ---- counters --------------------------------------------------------
    pub fn files_scanned(&self, n: usize) -> String {
        if self.es() {
            format!("Archivos escaneados: {n}")
        } else {
            format!("Files scanned: {n}")
        }
    }
    pub fn files_scanned_progress(&self, index: usize, total: usize) -> String {
        if self.es() {
            format!("Archivos escaneados: {index} / {total}")
        } else {
            format!("Files scanned: {index} / {total}")
        }
    }
    pub fn duplicates_found(&self, n: usize) -> String {
        if self.es() {
            format!("Duplicados encontrados: {n}")
        } else {
            format!("Duplicates found: {n}")
        }
    }
    pub fn duplicates_found_groups(&self, n: usize, groups: usize) -> String {
        if self.es() {
            format!("Duplicados encontrados: {n} ({groups} grupos)")
        } else {
            format!("Duplicates found: {n} ({groups} groups)")
        }
    }
    pub fn reclaimable(&self, human: &str) -> String {
        if self.es() {
            format!("Recuperable: {human}")
        } else {
            format!("Reclaimable: {human}")
        }
    }
    pub fn elapsed(&self, secs: f64, groups: usize, threads: usize) -> String {
        if self.es() {
            format!("Tiempo: {secs:.2}s · {threads} hilos · {groups} grupo(s)")
        } else {
            format!("Elapsed: {secs:.2}s · {threads} threads · {groups} group(s)")
        }
    }

    pub fn original_likely(&self, pct: f32) -> String {
        if self.es() {
            format!("Original · {pct:.0}% probable")
        } else {
            format!("Original · {pct:.0}% likely")
        }
    }

    pub fn reason_labels(&self, codes: &[String]) -> String {
        let labels: Vec<&'static str> = codes
            .iter()
            .map(|code| match (self.es(), code.as_str()) {
                (false, "resolution") => "higher resolution",
                (false, "quality") => "better quality per pixel/bitrate",
                (false, "age") => "older file date",
                (false, "metadata") => "camera EXIF metadata",
                (false, "name") => "name without copy markers",
                (false, "location") => "typical original location",
                (false, _) => "most consistent signal set",
                (true, "resolution") => "mayor resolución",
                (true, "quality") => "mejor calidad por píxel/bitrate",
                (true, "age") => "fecha de archivo más antigua",
                (true, "metadata") => "metadatos EXIF de cámara",
                (true, "name") => "nombre sin marcas de copia",
                (true, "location") => "ubicación típica de originales",
                (true, _) => "conjunto de señales más consistente",
            })
            .collect();
        labels.join(", ")
    }

    pub fn no_picture(&self) -> String {
        if self.es() {
            "Sin imagen seleccionada".into()
        } else {
            "No picture selected".into()
        }
    }

    pub fn status_stopped_partial(&self, n: usize) -> String {
        if self.es() {
            format!("Detenido tras {n} archivo(s) — resultados parciales conservados. Puedes volver a buscar.")
        } else {
            format!(
                "Stopped after {n} file(s) — partial results kept. You can start again."
            )
        }
    }
    pub fn status_stopped_zero(&self) -> String {
        if self.es() {
            "Detenido antes de procesar ningún archivo. Puedes volver a buscar.".into()
        } else {
            "Stopped before processing any file. You can start again.".into()
        }
    }
    pub fn status_finished(&self, groups: usize, duplicates: usize) -> String {
        if self.es() {
            format!("Terminado: {groups} grupo(s), {duplicates} duplicado(s).")
        } else {
            format!("Finished: {groups} group(s), {duplicates} duplicate(s).")
        }
    }
    pub fn browser_folders(&self, n: usize) -> String {
        if self.es() {
            format!("{n} subcarpeta(s) — pulsa una para abrirla")
        } else {
            format!("{n} sub-folder(s) — click one to open it")
        }
    }
    pub fn browser_not_a_folder(&self, path: &Path) -> String {
        if self.es() {
            format!("No es una carpeta: {}", path.display())
        } else {
            format!("Not a folder: {}", path.display())
        }
    }
    pub fn browser_destination(&self) -> String {
        if self.es() {
            "Elige la carpeta de destino y pulsa «Usar esta carpeta».".into()
        } else {
            "Choose the destination folder and press “Select this folder”.".into()
        }
    }
}

impl Tr {
    /// Formatted help content: 0 = heading, 1 = paragraph, 2 = bullet.
    pub fn help_lines(&self) -> Vec<HelpLine> {
        let raw: Vec<(&str, i32)> = if self.es() {
            vec![
                ("Qué hace UMDF", 0),
                ("UMDF busca fotos, vídeos, audio y archivos duplicados. Combina la comparación exacta byte a byte con huellas perceptuales y conserva la copia con más probabilidad de ser el original.", 1),
                ("Cómo funciona la detección", 0),
                ("• Exacto: mismo tamaño + SHA-256 (o comparación completa del archivo) → 100 %.", 2),
                ("• Perceptual: la imagen se reescala a 75×75 en gris; «dpf» cuenta los píxeles dentro de la tolerancia de gris.", 2),
                ("• Vídeo: metadatos de ffprobe + un fotograma en «Frame position», con la misma huella de imagen.", 2),
                ("• Audio: se hashean 5 s de PCM decodificado; los archivos re-muxeados o re-etiquetados coinciden, los re-codificados no.", 2),
                ("Tolerancia de gris (pixel tolerance)", 0),
                ("Es la diferencia máxima de nivel de gris (0–255) para que dos píxeles cuenten como iguales. 0 = estricto; 10 (por defecto) tolera la recompresión JPEG; 14–18 tolera retoques como la corrección de ojos rojos o pequeños recortes; por encima de ~24 aparecen falsos positivos.", 1),
                ("Si se escapan copias editadas, súbela poco a poco. Para archivos exactos, bájala.", 1),
                ("Métricas", 0),
                ("• dpf (por defecto): tolerancia de píxel; robusta a reescalado, compresión y pequeños retoques.", 2),
                ("• ssim: similitud estructural; especialmente robusta a retoques locales (ojos rojos) y compresión.", 2),
                ("• sign: acuerdo de signo de la rejilla normalizada.", 2),
                ("• pearson: correlación de las rejillas; más estricta con la estructura.", 2),
                ("• legacy: métrica de signo heredada, solo para comparación.", 2),
                ("• Semántica (opcional): embeddings ONNX (ResNet/CLIP) comparados por coseno; requiere compilar con --features embeddings.", 2),
                ("Fotos rotadas y editadas", 0),
                ("• Se aplica la orientación EXIF al decodificar: una foto con la etiqueta de rotación coincide con su copia rotada físicamente.", 2),
                ("• Una rotación «horneada» en los píxeles sin EXIF aún puede escaparse; revísala a mano.", 2),
                ("• La corrección de ojos rojos cambia un área mínima; «dpf» suele seguir coincidiendo. Si no, sube la tolerancia o baja el umbral de similitud.", 2),
                ("Qué copia se conserva", 0),
                ("La política «original» puntúa resolución, bytes por píxel, antigüedad del archivo, riqueza EXIF, marcas en el nombre (copy, edited…) y ubicación. La vista previa muestra la puntuación y los motivos.", 1),
                ("Acciones seguras", 0),
                ("• Papelera/Mover/Borrar actúan sobre un duplicado; los botones masivos piden confirmación.", 2),
                ("• Tras borrar, la fila siguiente queda seleccionada para comparar y borrar en cadena.", 2),
                ("• Teclado: ↑/↓ seleccionan, Supr/Delete borra.", 2),
                ("Rendimiento", 0),
                ("La decodificación, el hash y la comparación usan todas las CPU por defecto; ajusta «Threads» o --threads. Las vistas previas se decodifican fuera del hilo de interfaz.", 1),
                ("Limitaciones", 0),
                ("• El audio re-codificado no se detecta como el mismo contenido (por diseño).", 2),
                ("• Aún no hay similitud semántica/IA (CLIP/ResNet) ni aceleración por GPU.", 2),
            ]
        } else {
            vec![
                ("What UMDF does", 0),
                ("UMDF scans folders for duplicate photos, videos, audio and files. It combines exact byte comparison with perceptual fingerprints and keeps the copy most likely to be the original.", 1),
                ("How detection works", 0),
                ("• Exact: same size + SHA-256 (or full-file comparison) → 100%.", 2),
                ("• Perceptual: the image is resampled to 75×75 grayscale; 'dpf' counts pixels within the gray tolerance.", 2),
                ("• Video: ffprobe metadata + one frame at 'Frame position', fingerprinted as an image.", 2),
                ("• Audio: 5 s of decoded PCM hashed; re-muxed or retagged files match, re-encoded ones do not.", 2),
                ("Pixel tolerance (gray tolerance)", 0),
                ("Maximum difference in gray level (0–255) for two pixels to count as equal. 0 = strict; 10 (default) tolerates JPEG recompression; 14–18 tolerates retouching such as red-eye correction or small crops; above ~24 false positives appear.", 1),
                ("If edited copies are missed, raise it gradually. For exact archives, lower it.", 1),
                ("Metrics", 0),
                ("• dpf (default): pixel tolerance; robust to resize, compression and small edits.", 2),
                ("• ssim: structural similarity; especially robust to local retouching (red-eye) and compression.", 2),
                ("• sign: sign agreement of the normalised grid.", 2),
                ("• pearson: correlation of the grids; stricter about structure.", 2),
                ("• legacy: legacy sign-agreement metric kept for comparison.", 2),
                ("• Semantic (optional): ONNX embeddings (ResNet/CLIP) compared by cosine; requires a build with --features embeddings.", 2),
                ("Rotated and edited photos", 0),
                ("• EXIF orientation is applied when decoding, so a tagged rotated photo matches its physically rotated copy.", 2),
                ("• A rotation baked into the pixels without EXIF can still be missed; review those manually.", 2),
                ("• Red-eye correction changes a tiny area; 'dpf' usually still matches. If not, raise the tolerance or lower the similarity threshold.", 2),
                ("Which copy is kept?", 0),
                ("The 'original' policy scores resolution, bytes per pixel, file age, EXIF richness, name markers (copy, edited…) and folder location. The preview shows the score and reasons.", 1),
                ("Safe actions", 0),
                ("• Trash/Move/Delete act on one duplicate; bulk buttons ask for confirmation.", 2),
                ("• After deleting, the next row stays selected to compare and delete repeatedly.", 2),
                ("• Keyboard: ↑/↓ select, Del/Delete removes.", 2),
                ("Performance", 0),
                ("Decoding, hashing and comparison use all CPUs by default; adjust 'Threads' or --threads. Previews are decoded off the UI thread.", 1),
                ("Limitations", 0),
                ("• Re-encoded audio is not detected as the same content (by design).", 2),
                ("• No semantic/AI similarity (CLIP/ResNet) and no GPU acceleration yet.", 2),
            ]
        };
        raw.into_iter()
            .map(|(text, kind)| HelpLine {
                text: text.into(),
                kind,
            })
            .collect()
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
