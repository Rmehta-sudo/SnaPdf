use crate::model::{
    Annotation, AnnotationKind, AppState, RecentDocument, SidebarFileIcons, is_pdf,
};
use crate::ocr::{OcrCommand, OcrEvent, OcrWorker};
use crate::pdf::{
    LinkTarget, PdfFileSearchMatches, PdfLinkRegion, PdfOutlineItem, PdfSearchMatch, RenderCommand,
    RenderEvent, RenderWorker, TextGlyph, search_pdf_files,
};
use crate::{storage, theme};
use anyhow::{Result, anyhow};
use eframe::egui::{
    self, Align, Align2, Color32, ColorImage, CornerRadius, CursorIcon, FontId, Frame, Key,
    KeyboardShortcut, Layout, Margin, Modifiers, Rect, Response, RichText, ScrollArea, Sense,
    Stroke, StrokeKind, TextureHandle, TextureOptions, pos2, vec2,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

const MAX_TEXTURES: usize = 10;
const MAX_TEXTURE_BYTES: usize = 256 * 1024 * 1024;
const MAX_PENDING_PAGES: usize = 6;
const PAGE_GAP: f32 = 18.0;
const KEYBOARD_TAP_SCROLL: f32 = 150.0;
const WHEEL_SCROLL_MULTIPLIER: f32 = 1.65;

#[derive(Clone)]
enum RenameTarget {
    Project(usize),
    File(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct TextPosition {
    page: usize,
    glyph: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TextSelection {
    anchor: TextPosition,
    focus: TextPosition,
}

impl TextSelection {
    fn ordered(self) -> (TextPosition, TextPosition) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    // Return the selected glyph range on this page (including intermediate pages).
    fn bounds_on_page(self, page: usize, glyph_count: usize) -> Option<(usize, usize)> {
        if glyph_count == 0 {
            return None;
        }
        let (start, end) = self.ordered();
        if page < start.page || page > end.page {
            return None;
        }
        let first = if page == start.page {
            start.glyph.min(glyph_count - 1)
        } else {
            0
        };
        let last = if page == end.page {
            end.glyph.min(glyph_count - 1)
        } else {
            glyph_count - 1
        };
        (first <= last).then_some((first, last))
    }
}

#[derive(Clone)]
struct BrowserEntry {
    path: PathBuf,
    relative: PathBuf,
    is_directory: bool,
    pdf_count: usize,
}

struct BrowserScanResult {
    directory: PathBuf,
    entries: Vec<BrowserEntry>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SearchScope {
    OpenTabs,
    Workspace,
}

struct SearchScanResult {
    scope: SearchScope,
    query: String,
    matches: Vec<PdfFileSearchMatches>,
}

#[derive(Clone, Copy)]
struct PinchAnchor {
    page: usize,
    fraction: egui::Vec2,
}

fn poll_search_scan(receiver: &mut Option<Receiver<SearchScanResult>>) -> Option<SearchScanResult> {
    let result = match receiver.as_ref()?.try_recv() {
        Ok(result) => Some(result),
        Err(TryRecvError::Disconnected) => None,
        Err(TryRecvError::Empty) => return None,
    };
    *receiver = None;
    result
}

enum BreadcrumbAction {
    Browse(PathBuf),
    Copy(PathBuf),
}

#[derive(Clone, Copy)]
enum SelectionAction {
    Copy,
    Highlight,
}

struct AnnotationUiResult {
    action: Option<AnnotationContextAction>,
    hovered_comment: Option<u64>,
    clicked_comment: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolMode {
    Select,
    Highlight,
    Pen,
    Eraser,
    Note,
    TextBox,
    Rectangle,
    Ellipse,
    Signature,
    Stamp,
    Attachment,
    Ocr,
    MathOcr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidebarTab {
    Library,
    Contents,
    Search,
}

enum AnnotationContextAction {
    AddComment {
        page: usize,
        position: [f32; 2],
        parent: Option<u64>,
    },
    EditComment(u64),
    Delete(u64),
}

struct OcrPopup {
    text: String,
    detail: String,
    busy: bool,
}

struct NoteDraft {
    annotation_id: Option<u64>,
    parent: Option<u64>,
    page: usize,
    position: [f32; 2],
    screen_anchor: Option<egui::Pos2>,
    text: String,
}

struct TextDraft {
    page: usize,
    rect: [f32; 4],
    text: String,
    stamp: bool,
}

#[derive(Clone)]
struct ExternalApp {
    name: String,
    desktop_file: PathBuf,
}

struct PrintDialog {
    printers: Vec<String>,
    printer: String,
    page_range: String,
    copies: u32,
}

#[derive(Default)]
struct PdfTreeNode {
    name: String,
    path: PathBuf,
    directories: BTreeMap<String, PdfTreeNode>,
    files: Vec<PathBuf>,
}

enum TreeAction {
    Open(PathBuf),
    OpenWith(PathBuf),
    OpenInFiles(PathBuf),
    Rename(PathBuf, String),
    ResetName(PathBuf),
    PinFile(PathBuf),
    UnpinFile(PathBuf),
    PinFolder(PathBuf),
    UnpinFolder(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PageKey {
    index: usize,
    width: u16,
    generation: u64,
}

struct CachedPage {
    texture: TextureHandle,
    rendered_width: u16,
    generation: u64,
    byte_size: usize,
    last_used_frame: u64,
}

pub fn run() -> Result<()> {
    let initial_path = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .find(|path| path.is_file() && is_pdf(path));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("SnapPDF")
            .with_app_id("io.snappdf.SnapPDF")
            .with_inner_size([1360.0, 860.0])
            .with_min_inner_size([820.0, 560.0]),
        ..Default::default()
    };

    eframe::run_native(
        "SnapPDF",
        options,
        Box::new(move |creation| Ok(Box::new(SnapPdfApp::new(creation, initial_path)))),
    )
    .map_err(|error| anyhow!(error.to_string()))
}

pub struct SnapPdfApp {
    state: AppState,
    /// Frozen at startup so clicking a PDF never reshuffles the visible recent list.
    session_recents: Vec<RecentDocument>,
    worker: RenderWorker,
    ocr_worker: OcrWorker,
    project_files: HashMap<PathBuf, Vec<PathBuf>>,
    current_path: Option<PathBuf>,
    page_sizes: Vec<[f32; 2]>,
    textures: HashMap<usize, CachedPage>,
    pending_pages: HashSet<PageKey>,
    text_layers: HashMap<usize, Vec<TextGlyph>>,
    link_layers: HashMap<usize, Vec<PdfLinkRegion>>,
    pending_text: HashSet<(usize, u64)>,
    text_selection: Option<TextSelection>,
    text_selection_dragging: bool,
    pending_selection_highlight: bool,
    generation: u64,
    frame_number: u64,
    zoom: f32,
    current_page: usize,
    page_entry: String,
    page_entry_editing: bool,
    sidebar_search: String,
    status: String,
    renderer_ready: bool,
    fit_width_requested: bool,
    keyboard_scroll: f32,
    vertical_arrow_hold: f32,
    horizontal_arrow_hold: f32,
    horizontal_page_repeat: f32,
    project_shortcut_armed_until: Option<f64>,
    jump_to_page: Option<usize>,
    rename_target: Option<RenameTarget>,
    rename_buffer: String,
    rename_focus_requested: bool,
    tool_mode: ToolMode,
    drag_start: Option<egui::Pos2>,
    ink_points: Vec<[f32; 2]>,
    note_draft: Option<NoteDraft>,
    text_draft: Option<TextDraft>,
    ocr_popup: Option<OcrPopup>,
    next_ocr_request: u64,
    annotation_undo: Vec<Vec<Annotation>>,
    annotation_redo: Vec<Vec<Annotation>>,
    annotation_color: Color32,
    math_ocr_requests: HashSet<u64>,
    sidebar_tab: SidebarTab,
    sidebar_visible: bool,
    toolbar_visible: bool,
    reader_mode: bool,
    presentation_mode: bool,
    /// The document zoom to restore after leaving Reader or Presentation mode.
    mode_zoom_restore: Option<f32>,
    /// The chrome scale to restore after a temporary focus-mode zoom.
    mode_ui_scale_restore: Option<f32>,
    presentation_fit_height_requested: bool,
    presentation_fit_delay_frames: u8,
    outline: Vec<PdfOutlineItem>,
    collapsed_outline_items: HashSet<usize>,
    open_with_path: Option<PathBuf>,
    external_apps: Vec<ExternalApp>,
    open_with_error: Option<String>,
    eraser_gesture_active: bool,
    collapsed_directories: HashSet<PathBuf>,
    search_query: String,
    search_matches: Vec<PdfSearchMatch>,
    search_index: usize,
    search_navigation_index: usize,
    search_request_id: u64,
    tab_search_matches: Vec<PdfFileSearchMatches>,
    workspace_search_matches: Vec<PdfFileSearchMatches>,
    tab_search_scan: Option<Receiver<SearchScanResult>>,
    workspace_search_scan: Option<Receiver<SearchScanResult>>,
    search_current_expanded: bool,
    search_tabs_expanded: bool,
    search_project_expanded: bool,
    native_pinch: Option<crate::gestures::NativePinch>,
    pending_native_pinches: Vec<crate::gestures::NativePinchEvent>,
    native_pinch_active: bool,
    native_pinch_suppress_until: f64,
    page_rects: Vec<(usize, Rect)>,
    pinch_anchor: Option<PinchAnchor>,
    /// ScrollArea offset measured after its last layout pass. Pinch zoom uses
    /// this to position the *next* layout directly, rather than queueing a
    /// scroll delta that races the ScrollArea's own gesture handling.
    document_scroll_offset: egui::Vec2,
    show_search: bool,
    print_dialog: Option<PrintDialog>,
    browser_directory: Option<PathBuf>,
    browser_entries: Vec<BrowserEntry>,
    browser_scan: Option<Receiver<BrowserScanResult>>,
    pinned_comment: Option<u64>,
    show_view_settings: bool,
    confirm_save_to_source: bool,
    pending_source_save: Option<(PathBuf, PathBuf)>,
}

impl SnapPdfApp {
    fn new(creation: &eframe::CreationContext<'_>, initial_path: Option<PathBuf>) -> Self {
        theme::apply(&creation.egui_ctx);
        let mut state = storage::load();
        state.prune_missing();
        state.normalize_recents();
        let initial_path = initial_path
            .or_else(|| state.active_document.clone())
            .or_else(|| state.last_document.clone());
        if let Some(path) = initial_path.as_ref() {
            state.record_open(path);
        }
        creation
            .egui_ctx
            .set_zoom_factor(chrome_zoom(state.ui_scale, state.chrome_scale_ratio));
        let session_recents = state.recent_documents.clone();
        let project_files = state
            .projects
            .iter()
            .map(|project| (project.root.clone(), project.pdf_files()))
            .collect();

        let ocr_worker = OcrWorker::spawn(creation.egui_ctx.clone());
        let _ = ocr_worker.commands.send(OcrCommand::Warmup);
        let mut app = Self {
            state,
            session_recents,
            worker: RenderWorker::spawn(creation.egui_ctx.clone()),
            ocr_worker,
            project_files,
            current_path: None,
            page_sizes: Vec::new(),
            textures: HashMap::new(),
            pending_pages: HashSet::new(),
            text_layers: HashMap::new(),
            link_layers: HashMap::new(),
            pending_text: HashSet::new(),
            text_selection: None,
            text_selection_dragging: false,
            pending_selection_highlight: false,
            generation: 0,
            frame_number: 0,
            zoom: 1.0,
            current_page: 0,
            page_entry: "1".to_owned(),
            page_entry_editing: false,
            sidebar_search: String::new(),
            status: "Preparing PDF renderer...".to_owned(),
            renderer_ready: false,
            fit_width_requested: true,
            keyboard_scroll: 0.0,
            vertical_arrow_hold: 0.0,
            horizontal_arrow_hold: 0.0,
            horizontal_page_repeat: 0.0,
            project_shortcut_armed_until: None,
            jump_to_page: None,
            rename_target: None,
            rename_buffer: String::new(),
            rename_focus_requested: false,
            tool_mode: ToolMode::Select,
            drag_start: None,
            ink_points: Vec::new(),
            note_draft: None,
            text_draft: None,
            ocr_popup: None,
            next_ocr_request: 1,
            annotation_undo: Vec::new(),
            annotation_redo: Vec::new(),
            annotation_color: Color32::from_rgb(255, 211, 74),
            math_ocr_requests: HashSet::new(),
            sidebar_tab: SidebarTab::Library,
            sidebar_visible: true,
            toolbar_visible: true,
            reader_mode: false,
            presentation_mode: false,
            mode_zoom_restore: None,
            mode_ui_scale_restore: None,
            presentation_fit_height_requested: false,
            presentation_fit_delay_frames: 0,
            outline: Vec::new(),
            collapsed_outline_items: HashSet::new(),
            open_with_path: None,
            external_apps: Vec::new(),
            open_with_error: None,
            eraser_gesture_active: false,
            collapsed_directories: HashSet::new(),
            search_query: String::new(),
            search_matches: Vec::new(),
            search_index: 0,
            search_navigation_index: 0,
            search_request_id: 0,
            tab_search_matches: Vec::new(),
            workspace_search_matches: Vec::new(),
            tab_search_scan: None,
            workspace_search_scan: None,
            search_current_expanded: true,
            search_tabs_expanded: false,
            search_project_expanded: false,
            native_pinch: crate::gestures::listen(creation),
            pending_native_pinches: Vec::new(),
            native_pinch_active: false,
            native_pinch_suppress_until: 0.0,
            page_rects: Vec::new(),
            pinch_anchor: None,
            document_scroll_offset: egui::Vec2::ZERO,
            show_search: false,
            print_dialog: None,
            browser_directory: None,
            browser_entries: Vec::new(),
            browser_scan: None,
            pinned_comment: None,
            show_view_settings: false,
            confirm_save_to_source: false,
            pending_source_save: None,
        };

        if let Some(path) = initial_path {
            app.open_document(path);
        }
        app
    }

    fn open_document(&mut self, path: PathBuf) {
        if !path.is_file() || !is_pdf(&path) {
            self.status = "SnapPDF can only open PDF files.".to_owned();
            return;
        }

        if let Some(current) = self.current_path.as_ref() {
            self.state
                .reading_positions
                .insert(current.clone(), self.current_page);
        }
        let restored_page = self
            .state
            .reading_positions
            .get(&path)
            .copied()
            .unwrap_or(0);
        self.generation = self.generation.wrapping_add(1);
        self.worker.expect_generation(self.generation);
        self.current_path = Some(path.clone());
        self.browser_directory = None;
        self.browser_entries.clear();
        self.browser_scan = None;
        self.pinned_comment = None;
        self.page_sizes.clear();
        self.pinch_anchor = None;
        self.document_scroll_offset = egui::Vec2::ZERO;
        self.textures.clear();
        self.pending_pages.clear();
        self.text_layers.clear();
        self.link_layers.clear();
        self.outline.clear();
        self.collapsed_outline_items.clear();
        self.pending_text.clear();
        self.text_selection = None;
        self.text_selection_dragging = false;
        self.pending_selection_highlight = false;
        self.search_matches.clear();
        self.search_index = 0;
        self.search_navigation_index = 0;
        self.annotation_undo.clear();
        self.annotation_redo.clear();
        self.tool_mode = ToolMode::Select;
        self.current_page = restored_page;
        self.jump_to_page = Some(restored_page);
        self.zoom = 1.0;
        self.fit_width_requested = true;
        self.status = format!("Opening {}...", file_name(&path));
        self.state.record_open(&path);
        let _ = storage::save(&self.state);
        let _ = self.worker.commands.send(RenderCommand::Open {
            path,
            generation: self.generation,
        });
    }

    fn close_document_tab(&mut self, path: &Path) {
        if self.current_path.as_deref() == Some(path) {
            self.state
                .reading_positions
                .insert(path.to_path_buf(), self.current_page);
        }
        let index = self
            .state
            .open_documents
            .iter()
            .position(|open| open == path);
        self.state.open_documents.retain(|open| open != path);
        self.state.record_closed(path);
        if self.current_path.as_deref() == Some(path) {
            let next = index.and_then(|index| {
                self.state
                    .open_documents
                    .get(index.min(self.state.open_documents.len().saturating_sub(1)))
                    .cloned()
            });
            if let Some(next) = next {
                self.open_document(next);
            } else {
                self.current_path = None;
                self.page_sizes.clear();
                self.textures.clear();
                self.text_layers.clear();
                self.link_layers.clear();
                self.outline.clear();
                self.state.active_document = None;
            }
        }
        self.persist();
    }

    fn reopen_last_closed_tab(&mut self) {
        let Some(path) = self.state.take_most_recent_closed() else {
            self.status = "No recently closed PDF tabs.".to_owned();
            return;
        };
        let name = self.state.display_name(&path);
        self.open_document(path);
        self.status = format!("Reopened {name}.");
    }

    fn add_project_dialog(&mut self) {
        if let Some(folder) = rfd::FileDialog::new().pick_folder()
            && self.state.add_project(folder.clone())
        {
            let files = self
                .state
                .projects
                .iter()
                .find(|project| project.root == folder)
                .map(|project| project.pdf_files())
                .unwrap_or_default();
            self.project_files.insert(folder, files);
            self.persist();
        }
    }

    fn open_file_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("PDF document", &["pdf", "PDF"])
            .pick_file()
        {
            self.open_document(path);
        }
    }

    fn request_open_with(&mut self, path: PathBuf) {
        self.open_with_path = Some(path);
        self.external_apps = available_pdf_apps();
        self.open_with_error = self
            .external_apps
            .is_empty()
            .then(|| "No other registered PDF applications were found.".to_owned());
    }

    fn open_path_in_file_manager(&mut self, path: &Path) {
        let directory = if path.is_dir() {
            path.to_path_buf()
        } else {
            path.parent().unwrap_or(path).to_path_buf()
        };
        match Command::new("xdg-open").arg(&directory).spawn() {
            Ok(_) => self.status = format!("Opened {}.", directory.display()),
            Err(error) => self.status = format!("Could not open the folder: {error}"),
        }
    }

    fn open_current_directory(&mut self) {
        let directory = self.browser_directory.clone().or_else(|| {
            self.current_path
                .as_deref()
                .and_then(Path::parent)
                .map(Path::to_path_buf)
        });
        let Some(directory) = directory else {
            return;
        };
        self.open_path_in_file_manager(&directory);
    }

    fn launch_external_app(&mut self, app: &ExternalApp, path: &Path) {
        match Command::new("gio")
            .arg("launch")
            .arg(&app.desktop_file)
            .arg(path)
            .spawn()
        {
            Ok(_) => {
                self.status = format!("Opened in {}.", app.name);
                self.open_with_path = None;
            }
            Err(error) => {
                self.open_with_error = Some(format!("Could not open {}: {error}", app.name));
            }
        }
    }

    fn request_print(&mut self) {
        if self.current_path.is_none() {
            return;
        }
        let printers = installed_printers();
        let printer =
            default_printer().unwrap_or_else(|| printers.first().cloned().unwrap_or_default());
        self.print_dialog = Some(PrintDialog {
            printers,
            printer,
            page_range: String::new(),
            copies: 1,
        });
    }

    fn submit_print(&mut self) {
        let (Some(path), Some(dialog)) = (self.current_path.clone(), self.print_dialog.as_ref())
        else {
            return;
        };
        let mut command = Command::new("lp");
        if !dialog.printer.trim().is_empty() {
            command.arg("-d").arg(dialog.printer.trim());
        }
        command
            .arg("-n")
            .arg(dialog.copies.clamp(1, 99).to_string());
        if !dialog.page_range.trim().is_empty() {
            command.arg("-P").arg(dialog.page_range.trim());
        }
        command.arg(&path);
        match command.spawn() {
            Ok(_) => {
                self.status = "Print job submitted.".to_owned();
                self.print_dialog = None;
            }
            Err(error) => {
                self.status = format!("Could not start printing: {error}");
            }
        }
    }

    fn export_annotated_copy(&mut self) {
        let Some(source) = self.current_path.clone() else {
            return;
        };
        let suggested = source
            .file_stem()
            .and_then(|name| name.to_str())
            .map(|name| format!("{name}-annotated.pdf"))
            .unwrap_or_else(|| "annotated.pdf".to_owned());
        let Some(destination) = rfd::FileDialog::new()
            .add_filter("PDF document", &["pdf"])
            .set_file_name(&suggested)
            .save_file()
        else {
            return;
        };
        self.status = "Exporting annotated copy...".to_owned();
        let annotations = self.current_annotations().to_vec();
        let _ = self.worker.commands.send(RenderCommand::ExportAnnotated {
            source,
            destination,
            annotations,
            generation: self.generation,
        });
    }

    fn save_annotations_to_source(&mut self) {
        let Some(source) = self.current_path.clone() else {
            return;
        };
        let name = source
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("document.pdf");
        let temporary =
            source.with_file_name(format!(".{name}.snappdf-saving-{}.pdf", std::process::id()));
        let annotations = self.current_annotations().to_vec();
        self.pending_source_save = Some((temporary.clone(), source.clone()));
        self.status = "Writing annotations into the PDF...".to_owned();
        let _ = self.worker.commands.send(RenderCommand::ExportAnnotated {
            source,
            destination: temporary,
            annotations,
            generation: self.generation,
        });
    }

    fn refresh_projects(&mut self) {
        self.project_files = self
            .state
            .projects
            .iter()
            .map(|project| (project.root.clone(), project.pdf_files()))
            .collect();
        self.status = "Project folders refreshed.".to_owned();
    }

    fn open_folder_browser(&mut self, directory: PathBuf) {
        if !directory.is_dir() {
            self.status = "That folder is no longer available.".to_owned();
            return;
        }
        self.browser_directory = Some(directory.clone());
        self.browser_entries.clear();
        self.status = "Scanning nested folders for PDFs...".to_owned();
        let (sender, receiver) = mpsc::channel();
        self.browser_scan = Some(receiver);
        std::thread::spawn(move || {
            let mut entries = Vec::new();
            if let Ok(children) = fs::read_dir(&directory) {
                for child in children.flatten() {
                    let path = child.path();
                    if path.is_dir() {
                        entries.push(BrowserEntry {
                            relative: path
                                .strip_prefix(&directory)
                                .unwrap_or(path.as_path())
                                .to_path_buf(),
                            path,
                            is_directory: true,
                            pdf_count: 0,
                        });
                    }
                }
            }
            for entry in WalkDir::new(&directory)
                .follow_links(false)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_file() && is_pdf(entry.path()))
            {
                let path = entry.into_path();
                entries.push(BrowserEntry {
                    relative: path
                        .strip_prefix(&directory)
                        .unwrap_or(path.as_path())
                        .to_path_buf(),
                    path,
                    is_directory: false,
                    pdf_count: 1,
                });
            }
            let pdf_paths = entries
                .iter()
                .filter(|entry| !entry.is_directory)
                .map(|entry| entry.path.clone())
                .collect::<Vec<_>>();
            for entry in entries.iter_mut().filter(|entry| entry.is_directory) {
                entry.pdf_count = pdf_paths
                    .iter()
                    .filter(|path| path.starts_with(&entry.path))
                    .count();
            }
            entries.sort_by(|left, right| {
                right
                    .is_directory
                    .cmp(&left.is_directory)
                    .then_with(|| left.relative.cmp(&right.relative))
            });
            let _ = sender.send(BrowserScanResult { directory, entries });
        });
    }

    fn persist(&mut self) {
        if let Err(error) = storage::save(&self.state) {
            self.status = format!("Could not save app state: {error}");
        }
    }

    fn handle_events(&mut self, ctx: &egui::Context) {
        if let Some(pinch) = self.native_pinch.as_ref() {
            while let Ok(event) = pinch.events.try_recv() {
                self.pending_native_pinches.push(event);
            }
        }
        if let Some(result) = poll_search_scan(&mut self.tab_search_scan)
            && result.scope == SearchScope::OpenTabs
            && result.query == self.search_query.trim()
        {
            self.tab_search_matches = result.matches;
        }
        if let Some(result) = poll_search_scan(&mut self.workspace_search_scan)
            && result.scope == SearchScope::Workspace
            && result.query == self.search_query.trim()
        {
            self.workspace_search_matches = result.matches;
        }
        while let Ok(event) = self.worker.events.try_recv() {
            match event {
                RenderEvent::Ready => {
                    self.renderer_ready = true;
                    if self.current_path.is_none() {
                        self.status = "Ready".to_owned();
                    }
                }
                RenderEvent::DocumentOpened {
                    path,
                    generation,
                    page_sizes,
                    outline,
                } if generation == self.generation => {
                    self.page_sizes = page_sizes;
                    self.outline = outline;
                    self.status = format!("{} - {} pages", file_name(&path), self.page_sizes.len());
                    self.fit_width_requested = true;
                }
                RenderEvent::PageRendered {
                    page_index,
                    generation,
                    target_width,
                    width,
                    height,
                    rgba,
                } if generation == self.generation => {
                    let key = PageKey {
                        index: page_index,
                        width: target_width,
                        generation,
                    };
                    self.pending_pages.remove(&key);
                    let byte_size = rgba.len();
                    let image = ColorImage::from_rgba_unmultiplied([width, height], &rgba);
                    let texture = ctx.load_texture(
                        format!("page-{generation}-{page_index}-{target_width}"),
                        image,
                        TextureOptions::LINEAR,
                    );
                    let should_replace = self.textures.get(&page_index).is_none_or(|page| {
                        page.generation != generation || target_width >= page.rendered_width
                    });
                    if should_replace {
                        self.textures.insert(
                            page_index,
                            CachedPage {
                                texture,
                                rendered_width: target_width,
                                generation,
                                byte_size,
                                last_used_frame: self.frame_number,
                            },
                        );
                    }
                    self.trim_texture_cache();
                }
                RenderEvent::TextExtracted {
                    page_index,
                    generation,
                    glyphs,
                    mut links,
                } if generation == self.generation => {
                    self.pending_text.remove(&(page_index, generation));
                    links.extend(infer_text_links(&glyphs, &self.outline, &links));
                    self.text_layers.insert(page_index, glyphs);
                    self.link_layers.insert(page_index, links);
                }
                RenderEvent::SearchCompleted {
                    generation,
                    request_id,
                    query,
                    matches,
                } if generation == self.generation
                    && request_id == self.search_request_id
                    && query == self.search_query =>
                {
                    self.search_matches = matches;
                    self.search_index = 0;
                    if let Some(first) = self.search_matches.first() {
                        self.jump_to_page = Some(first.page_index);
                    }
                    self.status = if self.search_matches.is_empty() {
                        format!("No matches for “{}”.", self.search_query)
                    } else {
                        format!("{} matches.", self.search_matches.len())
                    };
                }
                RenderEvent::ExportCompleted {
                    generation,
                    destination,
                    exported,
                    skipped,
                } if generation == self.generation => {
                    let source_save = self
                        .pending_source_save
                        .take()
                        .filter(|(temporary, _)| *temporary == destination);
                    if let Some((temporary, source)) = source_save {
                        let timestamp = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|duration| duration.as_secs())
                            .unwrap_or_default();
                        let stem = source
                            .file_stem()
                            .and_then(|name| name.to_str())
                            .unwrap_or("document");
                        let backup =
                            source.with_file_name(format!("{stem}.snappdf-backup-{timestamp}.pdf"));
                        match fs::copy(&source, &backup)
                            .and_then(|_| fs::rename(&temporary, &source))
                        {
                            Ok(_) => {
                                self.state.annotations.remove(&source);
                                self.persist();
                                self.status = format!(
                                    "Saved {exported} annotations to the PDF. Backup: {}",
                                    backup.display()
                                );
                                self.open_document(source);
                            }
                            Err(error) => {
                                self.status = format!(
                                    "Could not replace the original PDF: {error}. The original was not changed."
                                );
                            }
                        }
                    } else {
                        self.status = if skipped == 0 {
                            format!(
                                "Exported {exported} PDF annotations to {}.",
                                destination.display()
                            )
                        } else {
                            format!(
                                "Exported {exported} annotations to {}; {skipped} unsupported items remain SnapPDF overlays.",
                                destination.display()
                            )
                        };
                    }
                }
                RenderEvent::RegionRendered {
                    generation,
                    request_id,
                    width,
                    height,
                    rgb,
                } if generation == self.generation => {
                    let math = self.math_ocr_requests.remove(&request_id);
                    let _ = self.ocr_worker.commands.send(OcrCommand::Recognize {
                        request_id,
                        width,
                        height,
                        rgb,
                        math,
                    });
                    if let Some(popup) = self.ocr_popup.as_mut() {
                        popup.detail = if math {
                            "Converting formula to LaTeX locally; first use may finish the model download...".to_owned()
                        } else {
                            "Reading image locally with PaddleOCR...".to_owned()
                        };
                    }
                }
                RenderEvent::Error {
                    generation,
                    message,
                } if generation == 0 || generation == self.generation => {
                    if let Some((temporary, _)) = self.pending_source_save.take() {
                        let _ = fs::remove_file(temporary);
                    }
                    self.status = message;
                }
                _ => {}
            }
        }
        while let Ok(event) = self.ocr_worker.events.try_recv() {
            match event {
                OcrEvent::Ready => {
                    if self.status == "Preparing local OCR..." {
                        self.status = "Ready".to_owned();
                    }
                }
                OcrEvent::Result {
                    request_id,
                    text,
                    engine,
                    elapsed_ms,
                } => {
                    self.ocr_popup = Some(OcrPopup {
                        text,
                        detail: format!(
                            "{engine} - {elapsed_ms} ms - processed locally (request {request_id})"
                        ),
                        busy: false,
                    });
                }
                OcrEvent::Error {
                    request_id,
                    message,
                } => {
                    if request_id.is_some() {
                        self.ocr_popup = Some(OcrPopup {
                            text: String::new(),
                            detail: message,
                            busy: false,
                        });
                    } else {
                        self.status =
                            "Local OCR unavailable; PDF text snips still work.".to_owned();
                    }
                }
            }
        }
        let scan_result = self
            .browser_scan
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok());
        if let Some(result) = scan_result {
            if self.browser_directory.as_deref() == Some(result.directory.as_path()) {
                self.browser_entries = result.entries;
                self.status = format!("{} PDF entries found.", self.browser_entries.len());
            }
            self.browser_scan = None;
        }
    }

    fn trim_texture_cache(&mut self) {
        while self.textures.len() > MAX_TEXTURES
            || self
                .textures
                .values()
                .map(|page| page.byte_size)
                .sum::<usize>()
                > MAX_TEXTURE_BYTES
        {
            let Some(oldest) = self
                .textures
                .iter()
                .min_by_key(|(_, page)| page.last_used_frame)
                .map(|(index, _)| *index)
            else {
                break;
            };
            self.textures.remove(&oldest);
        }
    }

    fn request_page(&mut self, key: PageKey) {
        let has_sharp_enough_page = self.textures.get(&key.index).is_some_and(|page| {
            page.generation == key.generation
                && page.rendered_width as f32 >= key.width as f32 * 0.92
        });
        let has_equal_or_better_request = self.pending_pages.iter().any(|pending| {
            pending.index == key.index
                && pending.generation == key.generation
                && pending.width >= key.width
        });
        if has_sharp_enough_page
            || has_equal_or_better_request
            || self.pending_pages.len() >= MAX_PENDING_PAGES
            || !self.pending_pages.insert(key)
        {
            return;
        }
        let _ = self.worker.commands.send(RenderCommand::RenderPage {
            page_index: key.index,
            target_width: key.width,
            generation: key.generation,
        });
    }

    fn request_text(&mut self, page_index: usize, generation: u64) {
        if self.text_layers.contains_key(&page_index)
            || !self.pending_text.insert((page_index, generation))
        {
            return;
        }
        let _ = self.worker.commands.send(RenderCommand::ExtractText {
            page_index,
            generation,
        });
    }

    fn run_search(&mut self, ctx: &egui::Context) {
        self.search_request_id = self.search_request_id.wrapping_add(1);
        self.search_matches.clear();
        self.search_index = 0;
        self.search_navigation_index = 0;
        self.tab_search_matches.clear();
        self.workspace_search_matches.clear();
        if self.search_query.trim().is_empty() {
            return;
        }
        self.status = format!("Searching for “{}”...", self.search_query);
        let _ = self.worker.commands.send(RenderCommand::Search {
            query: self.search_query.clone(),
            generation: self.generation,
            request_id: self.search_request_id,
        });
        self.start_scoped_search(SearchScope::OpenTabs, ctx);
        self.start_scoped_search(SearchScope::Workspace, ctx);
    }

    fn start_scoped_search(&mut self, scope: SearchScope, ctx: &egui::Context) {
        let query = self.search_query.trim().to_owned();
        if query.is_empty() {
            return;
        }
        let paths = match scope {
            SearchScope::OpenTabs => self.state.open_documents.clone(),
            SearchScope::Workspace => self.current_project_pdf_paths(),
        };
        let (sender, receiver) = mpsc::channel();
        match scope {
            SearchScope::OpenTabs => self.tab_search_scan = Some(receiver),
            SearchScope::Workspace => self.workspace_search_scan = Some(receiver),
        }
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let matches = search_pdf_files(&paths, &query, 40).unwrap_or_default();
            let _ = sender.send(SearchScanResult {
                scope,
                query,
                matches,
            });
            repaint.request_repaint();
        });
    }

    fn current_project_pdf_paths(&self) -> Vec<PathBuf> {
        let Some(current) = self.current_path.as_ref() else {
            return Vec::new();
        };
        let root = self
            .state
            .projects
            .iter()
            .filter(|project| current.starts_with(&project.root))
            .max_by_key(|project| project.root.components().count())
            .map(|project| project.root.clone())
            .or_else(|| current.parent().map(Path::to_path_buf));
        let Some(root) = root else {
            return Vec::new();
        };
        let mut paths = WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file() && is_pdf(entry.path()))
            .map(|entry| entry.into_path())
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }

    fn apply_pinch_zoom(&mut self, factor: f32, cursor: Option<egui::Pos2>, ctx: &egui::Context) {
        if !factor.is_finite() || factor <= 0.0 || (factor - 1.0).abs() <= 0.0001 {
            return;
        }
        if self.presentation_mode
            && factor < 1.0
            && self.zoom <= self.mode_zoom_restore.unwrap_or(self.zoom)
        {
            return;
        }
        self.pinch_anchor = cursor.and_then(|cursor| {
            // A pinch can be centred on the grey margin or between pages.
            // Use the nearest point *on* a page rather than dropping the anchor.
            let (page, rect) = self.page_rects.iter().min_by(|(_, a), (_, b)| {
                let distance_sq = |rect: &Rect| {
                    let dx = (rect.left() - cursor.x).max(0.0) + (cursor.x - rect.right()).max(0.0);
                    let dy = (rect.top() - cursor.y).max(0.0) + (cursor.y - rect.bottom()).max(0.0);
                    dx * dx + dy * dy
                };
                distance_sq(a).total_cmp(&distance_sq(b))
            })?;
            let on_page = pos2(
                cursor.x.clamp(rect.left(), rect.right()),
                cursor.y.clamp(rect.top(), rect.bottom()),
            );
            Some(PinchAnchor {
                page: *page,
                fraction: vec2(
                    (on_page.x - rect.left()) / rect.width().max(1.0),
                    (on_page.y - rect.top()) / rect.height().max(1.0),
                ),
            })
        });
        let minimum = if self.presentation_mode {
            self.mode_zoom_restore.unwrap_or(self.zoom)
        } else {
            0.2
        };
        self.zoom = (self.zoom * factor).clamp(minimum, 8.0);
        self.fit_width_requested = false;
        ctx.request_repaint();
    }

    fn step_search(&mut self, direction: isize) {
        let mut targets = Vec::new();
        if let Some(path) = self.current_path.as_ref() {
            targets.extend(
                self.search_matches
                    .iter()
                    .enumerate()
                    .map(|(index, result)| (path.clone(), result.page_index, Some(index))),
            );
        }
        for file in &self.tab_search_matches {
            targets.extend(
                file.matches
                    .iter()
                    .map(|result| (file.path.clone(), result.page_index, None)),
            );
        }
        for file in &self.workspace_search_matches {
            targets.extend(
                file.matches
                    .iter()
                    .map(|result| (file.path.clone(), result.page_index, None)),
            );
        }
        if targets.is_empty() {
            return;
        }
        let len = targets.len() as isize;
        self.search_navigation_index =
            (self.search_navigation_index as isize + direction).rem_euclid(len) as usize;
        let (path, page, current_match) = targets[self.search_navigation_index].clone();
        if self.current_path.as_deref() == Some(path.as_path()) {
            if let Some(index) = current_match {
                self.search_index = index;
            }
            self.jump_to_page = Some(page);
        } else {
            self.open_document(path);
            self.jump_to_page = Some(page);
        }
        self.status = format!(
            "Match {} of {} · page {}",
            self.search_navigation_index + 1,
            targets.len(),
            page + 1
        );
    }

    fn search_navigation_len(&self) -> usize {
        self.search_matches.len()
            + self
                .tab_search_matches
                .iter()
                .map(|file| file.matches.len())
                .sum::<usize>()
            + self
                .workspace_search_matches
                .iter()
                .map(|file| file.matches.len())
                .sum::<usize>()
    }

    fn set_reader_mode(&mut self, ctx: &egui::Context, enabled: bool) {
        if enabled {
            if self.mode_zoom_restore.is_none() {
                self.mode_zoom_restore = Some(self.zoom);
                self.mode_ui_scale_restore = Some(self.state.ui_scale);
            }
            self.reader_mode = true;
            self.sidebar_visible = false;
            self.toolbar_visible = false;
        } else if !self.presentation_mode {
            self.reader_mode = false;
            self.sidebar_visible = true;
            self.toolbar_visible = true;
            if let Some(zoom) = self.mode_zoom_restore.take() {
                self.zoom = zoom;
            }
            if let Some(scale) = self.mode_ui_scale_restore.take() {
                self.state.ui_scale = scale;
                ctx.set_zoom_factor(chrome_zoom(scale, self.state.chrome_scale_ratio));
                self.persist();
            }
        }
    }

    fn set_presentation_mode(&mut self, ctx: &egui::Context, enabled: bool) {
        if enabled {
            if self.mode_zoom_restore.is_none() {
                self.mode_zoom_restore = Some(self.zoom);
                self.mode_ui_scale_restore = Some(self.state.ui_scale);
            }
            self.presentation_mode = true;
            self.presentation_fit_height_requested = true;
            self.presentation_fit_delay_frames = 1;
            self.set_reader_mode(ctx, true);
        } else {
            self.presentation_mode = false;
            self.reader_mode = false;
            self.sidebar_visible = true;
            self.toolbar_visible = true;
            if let Some(zoom) = self.mode_zoom_restore.take() {
                self.zoom = zoom;
            }
            if let Some(scale) = self.mode_ui_scale_restore.take() {
                self.state.ui_scale = scale;
                ctx.set_zoom_factor(chrome_zoom(scale, self.state.chrome_scale_ratio));
                self.persist();
            }
            self.presentation_fit_height_requested = false;
            self.presentation_fit_delay_frames = 0;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(enabled));
    }

    fn keyboard_shortcuts(&mut self, ctx: &egui::Context) {
        let open = KeyboardShortcut::new(Modifiers::CTRL, Key::O);
        let new_project_prefix = KeyboardShortcut::new(Modifiers::CTRL, Key::K);
        let zoom_in = KeyboardShortcut::new(Modifiers::CTRL, Key::Plus);
        let zoom_out = KeyboardShortcut::new(Modifiers::CTRL, Key::Minus);
        let copy = KeyboardShortcut::new(Modifiers::CTRL, Key::C);
        let find = KeyboardShortcut::new(Modifiers::CTRL, Key::F);
        let print = KeyboardShortcut::new(Modifiers::CTRL, Key::P);
        let close_tab = KeyboardShortcut::new(Modifiers::CTRL, Key::W);
        let reopen_tab = KeyboardShortcut::new(Modifiers::CTRL | Modifiers::SHIFT, Key::T);
        let next_tab = KeyboardShortcut::new(Modifiers::CTRL, Key::Tab);
        let previous_tab = KeyboardShortcut::new(Modifiers::CTRL | Modifiers::SHIFT, Key::Tab);
        let undo = KeyboardShortcut::new(Modifiers::CTRL, Key::Z);
        let redo = KeyboardShortcut::new(Modifiers::CTRL | Modifiers::SHIFT, Key::Z);

        if self.presentation_mode {
            let (exit, previous, next, zoom_in_pressed, zoom_out_pressed) =
                ctx.input_mut(|input| {
                    (
                        input.consume_key(Modifiers::NONE, Key::Escape),
                        input.consume_key(Modifiers::NONE, Key::Backspace)
                            || input.consume_key(Modifiers::NONE, Key::ArrowLeft)
                            || input.consume_key(Modifiers::NONE, Key::K),
                        input.consume_key(Modifiers::NONE, Key::Space)
                            || input.consume_key(Modifiers::NONE, Key::ArrowRight)
                            || input.consume_key(Modifiers::NONE, Key::J),
                        input.consume_shortcut(&zoom_in),
                        input.consume_shortcut(&zoom_out),
                    )
                });
            if exit {
                self.set_presentation_mode(ctx, false);
            } else if previous {
                self.current_page = self.current_page.saturating_sub(1);
                self.jump_to_page = None;
                self.presentation_fit_height_requested = true;
                self.presentation_fit_delay_frames = 1;
            } else if next && !self.page_sizes.is_empty() {
                self.current_page =
                    (self.current_page + 1).min(self.page_sizes.len().saturating_sub(1));
                self.jump_to_page = None;
                self.presentation_fit_height_requested = true;
                self.presentation_fit_delay_frames = 1;
            } else if zoom_in_pressed {
                self.zoom = (self.zoom * 1.1).min(8.0);
            } else if zoom_out_pressed {
                self.zoom = (self.zoom / 1.1).max(self.mode_zoom_restore.unwrap_or(self.zoom));
            }
            return;
        }

        if self.reader_mode
            && ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape))
        {
            self.set_reader_mode(ctx, false);
            return;
        }

        if ctx.input_mut(|input| input.consume_shortcut(&new_project_prefix)) {
            self.project_shortcut_armed_until = Some(ctx.input(|input| input.time + 1.5));
            self.status = "Ctrl+K pressed — press Ctrl+O to add a project folder.".to_owned();
        }
        let project_shortcut_armed = self
            .project_shortcut_armed_until
            .is_some_and(|until| ctx.input(|input| input.time) <= until);
        if ctx.input_mut(|input| input.consume_shortcut(&open)) {
            if project_shortcut_armed {
                self.project_shortcut_armed_until = None;
                self.add_project_dialog();
            } else {
                self.open_file_dialog();
            }
        }
        if self
            .project_shortcut_armed_until
            .is_some_and(|until| ctx.input(|input| input.time) > until)
        {
            self.project_shortcut_armed_until = None;
        }
        if ctx.input_mut(|input| input.consume_shortcut(&zoom_in)) {
            let previous = self.state.ui_scale;
            self.state.ui_scale = (self.state.ui_scale + 0.1).min(5.0);
            self.zoom *= self.state.ui_scale / previous.max(0.1);
            ctx.set_zoom_factor(chrome_zoom(
                self.state.ui_scale,
                self.state.chrome_scale_ratio,
            ));
            if !self.reader_mode {
                self.persist();
            }
        }
        if ctx.input_mut(|input| input.consume_shortcut(&zoom_out)) {
            let previous = self.state.ui_scale;
            self.state.ui_scale = (self.state.ui_scale - 0.1).max(0.7);
            self.zoom *= self.state.ui_scale / previous.max(0.1);
            ctx.set_zoom_factor(chrome_zoom(
                self.state.ui_scale,
                self.state.chrome_scale_ratio,
            ));
            if !self.reader_mode {
                self.persist();
            }
        }
        if ctx.input_mut(|input| input.consume_shortcut(&copy))
            && let Some(text) = self.selected_text()
        {
            ctx.copy_text(text);
            self.status = "Selected text copied.".to_owned();
        }
        if ctx.input_mut(|input| input.consume_shortcut(&find)) {
            self.show_search = true;
            self.sidebar_tab = SidebarTab::Search;
        }
        if ctx.input_mut(|input| input.consume_shortcut(&print)) {
            self.request_print();
        }
        let tab_direction = ctx.input_mut(|input| {
            if input.consume_shortcut(&previous_tab) {
                Some(-1_isize)
            } else if input.consume_shortcut(&next_tab) {
                Some(1_isize)
            } else {
                None
            }
        });
        if let Some(direction) = tab_direction
            && self.state.open_documents.len() > 1
        {
            let documents = self.state.open_documents.clone();
            let current = self
                .current_path
                .as_ref()
                .and_then(|path| documents.iter().position(|document| document == path))
                .unwrap_or(0);
            let next = (current as isize + direction).rem_euclid(documents.len() as isize) as usize;
            self.open_document(documents[next].clone());
        }
        if !ctx.egui_wants_keyboard_input()
            && ctx.input_mut(|input| input.consume_shortcut(&close_tab))
            && let Some(path) = self.current_path.clone()
        {
            self.close_document_tab(&path);
        }
        if !ctx.egui_wants_keyboard_input()
            && ctx.input_mut(|input| input.consume_shortcut(&reopen_tab))
        {
            self.reopen_last_closed_tab();
        }
        if ctx.input_mut(|input| input.consume_shortcut(&undo)) {
            self.undo_annotation();
        }
        if ctx.input_mut(|input| input.consume_shortcut(&redo)) {
            self.redo_annotation();
        }

        if !ctx.egui_wants_keyboard_input() && !self.page_sizes.is_empty() {
            let (
                up_pressed,
                down_pressed,
                up_down,
                down_down,
                left_pressed,
                right_pressed,
                left_down,
                right_down,
                dt,
            ) = ctx.input_mut(|input| {
                (
                    input.consume_key(Modifiers::NONE, Key::ArrowUp),
                    input.consume_key(Modifiers::NONE, Key::ArrowDown),
                    input.key_down(Key::ArrowUp),
                    input.key_down(Key::ArrowDown),
                    input.consume_key(Modifiers::NONE, Key::ArrowLeft),
                    input.consume_key(Modifiers::NONE, Key::ArrowRight),
                    input.key_down(Key::ArrowLeft),
                    input.key_down(Key::ArrowRight),
                    input.stable_dt.min(0.05),
                )
            });
            if up_pressed {
                self.keyboard_scroll += KEYBOARD_TAP_SCROLL;
            }
            if down_pressed {
                self.keyboard_scroll -= KEYBOARD_TAP_SCROLL;
            }
            if up_down ^ down_down {
                self.vertical_arrow_hold += dt;
                let speed = if self.vertical_arrow_hold > 0.8 {
                    3_200.0
                } else if self.vertical_arrow_hold > 0.3 {
                    1_750.0
                } else {
                    850.0
                };
                self.keyboard_scroll += if up_down { speed * dt } else { -speed * dt };
                ctx.request_repaint();
            } else {
                self.vertical_arrow_hold = 0.0;
            }
            if left_pressed {
                self.jump_to_page = Some(self.current_page.saturating_sub(1));
            }
            if right_pressed {
                self.jump_to_page =
                    Some((self.current_page + 1).min(self.page_sizes.len().saturating_sub(1)));
            }
            if left_down ^ right_down {
                self.horizontal_arrow_hold += dt;
                if self.horizontal_arrow_hold > 0.35 {
                    self.horizontal_page_repeat += dt;
                    if self.horizontal_page_repeat >= 0.075 {
                        self.horizontal_page_repeat = 0.0;
                        self.jump_to_page = Some(if left_down {
                            self.current_page.saturating_sub(1)
                        } else {
                            (self.current_page + 1).min(self.page_sizes.len().saturating_sub(1))
                        });
                    }
                    ctx.request_repaint();
                }
            } else {
                self.horizontal_arrow_hold = 0.0;
                self.horizontal_page_repeat = 0.0;
            }
        } else {
            self.vertical_arrow_hold = 0.0;
            self.horizontal_arrow_hold = 0.0;
            self.horizontal_page_repeat = 0.0;
        }
    }

    fn current_annotations(&self) -> &[Annotation] {
        self.current_path
            .as_ref()
            .and_then(|path| self.state.annotations.get(path))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    fn push_annotation_history(&mut self) {
        self.annotation_undo
            .push(self.current_annotations().to_vec());
        if self.annotation_undo.len() > 64 {
            self.annotation_undo.remove(0);
        }
        self.annotation_redo.clear();
    }

    fn add_annotation(&mut self, page: usize, kind: AnnotationKind) {
        let Some(path) = self.current_path.clone() else {
            return;
        };
        self.push_annotation_history();
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        self.state
            .annotations
            .entry(path)
            .or_default()
            .push(Annotation { id, page, kind });
        self.persist();
    }

    fn remove_annotation(&mut self, id: u64, record_history: bool) -> bool {
        let Some(path) = self.current_path.clone() else {
            return false;
        };
        let exists = self
            .state
            .annotations
            .get(&path)
            .is_some_and(|items| items.iter().any(|annotation| annotation.id == id));
        if !exists {
            return false;
        }
        if record_history {
            self.push_annotation_history();
        }
        if let Some(items) = self.state.annotations.get_mut(&path) {
            items.retain(|annotation| {
                annotation.id != id
                    && !matches!(
                        &annotation.kind,
                        AnnotationKind::Note {
                            parent: Some(parent),
                            ..
                        } if *parent == id
                    )
            });
        }
        self.persist();
        true
    }

    fn update_comment(&mut self, id: u64, text: String) -> bool {
        let Some(path) = self.current_path.clone() else {
            return false;
        };
        let exists = self.state.annotations.get(&path).is_some_and(|items| {
            items.iter().any(|annotation| {
                annotation.id == id && matches!(annotation.kind, AnnotationKind::Note { .. })
            })
        });
        if !exists {
            return false;
        }
        self.push_annotation_history();
        if let Some(items) = self.state.annotations.get_mut(&path)
            && let Some(annotation) = items.iter_mut().find(|annotation| annotation.id == id)
            && let AnnotationKind::Note { text: saved, .. } = &mut annotation.kind
        {
            *saved = text;
        }
        self.persist();
        true
    }

    fn replace_annotations(&mut self, annotations: Vec<Annotation>) {
        let Some(path) = self.current_path.clone() else {
            return;
        };
        self.state.annotations.insert(path, annotations);
        self.persist();
    }

    fn undo_annotation(&mut self) {
        let Some(previous) = self.annotation_undo.pop() else {
            return;
        };
        self.annotation_redo
            .push(self.current_annotations().to_vec());
        self.replace_annotations(previous);
        self.status = "Annotation undone.".to_owned();
    }

    fn redo_annotation(&mut self) {
        let Some(next) = self.annotation_redo.pop() else {
            return;
        };
        self.annotation_undo
            .push(self.current_annotations().to_vec());
        self.replace_annotations(next);
        self.status = "Annotation restored.".to_owned();
    }

    fn dropped_files(&mut self, ctx: &egui::Context) {
        let files = ctx.input(|input| input.raw.dropped_files.clone());
        if let Some(path) = files
            .into_iter()
            .filter_map(|file| file.path)
            .find(|path| is_pdf(path))
        {
            self.open_document(path);
        }
    }

    fn navigation_rail(&mut self, root: &mut egui::Ui) {
        if self.reader_mode || self.presentation_mode {
            return;
        }
        egui::Panel::left("navigation-rail")
            .resizable(false)
            .exact_size(48.0)
            .frame(
                Frame::new()
                    .fill(Color32::from_rgb(25, 25, 25))
                    .inner_margin(Margin::symmetric(4, 10)),
            )
            .show(root, |ui| {
                ui.with_layout(Layout::top_down(Align::Center), |ui| {
                    let files = rail_button(
                        ui,
                        self.sidebar_visible && self.sidebar_tab == SidebarTab::Library,
                        RailIcon::Files,
                    )
                    .on_hover_text("Files")
                    .clicked();
                    if files {
                        if self.sidebar_visible && self.sidebar_tab == SidebarTab::Library {
                            self.sidebar_visible = false;
                        } else {
                            self.sidebar_visible = true;
                            self.sidebar_tab = SidebarTab::Library;
                        }
                    }
                    let contents = rail_button(
                        ui,
                        self.sidebar_visible && self.sidebar_tab == SidebarTab::Contents,
                        RailIcon::Contents,
                    )
                    .on_hover_text("Document outline")
                    .clicked();
                    if contents {
                        if self.sidebar_visible && self.sidebar_tab == SidebarTab::Contents {
                            self.sidebar_visible = false;
                        } else {
                            self.sidebar_visible = true;
                            self.sidebar_tab = SidebarTab::Contents;
                        }
                    }
                    let search = rail_button(
                        ui,
                        self.sidebar_visible && self.sidebar_tab == SidebarTab::Search,
                        RailIcon::Search,
                    )
                    .on_hover_text("Find in document (Ctrl+F)")
                    .clicked();
                    if search {
                        if self.sidebar_visible && self.sidebar_tab == SidebarTab::Search {
                            self.sidebar_visible = false;
                            self.show_search = false;
                        } else {
                            self.sidebar_visible = true;
                            self.show_search = true;
                            self.sidebar_tab = SidebarTab::Search;
                        }
                    }
                });
                ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
                    if rail_button(ui, false, RailIcon::Settings)
                        .on_hover_text("View settings")
                        .clicked()
                    {
                        self.show_view_settings = true;
                    }
                });
            });
    }

    fn sidebar(&mut self, root: &mut egui::Ui) {
        if !self.sidebar_visible || self.reader_mode || self.presentation_mode {
            return;
        }
        egui::Panel::left("project-sidebar")
            .resizable(true)
            .default_size(340.0)
            .frame(
                Frame::new()
                    .fill(theme::SIDEBAR_BG)
                    .stroke(Stroke::new(1.0, Color32::from_rgb(48, 48, 48)))
                    .inner_margin(Margin::same(10)),
            )
            .show(root, |ui| {
                // The document uses the global zoom; the explorer has its own readable scale.
                let explorer_factor = (self.state.sidebar_scale_ratio
                    / self.state.chrome_scale_ratio.max(0.1))
                .clamp(0.1, 4.0);
                ui.style_mut().spacing.item_spacing *= explorer_factor;
                ui.style_mut().spacing.button_padding *= explorer_factor;
                ui.style_mut().spacing.interact_size *= explorer_factor;
                for font in ui.style_mut().text_styles.values_mut() {
                    font.size *= explorer_factor;
                }
                ui.horizontal(|ui| {
                    if ui
                        .button("Open")
                        .on_hover_text("Open a PDF (Ctrl+O)")
                        .clicked()
                    {
                        self.open_file_dialog();
                    }
                    ui.separator();
                    if ui
                        .button("+ Project")
                        .on_hover_text("Add a PDF project folder")
                        .clicked()
                    {
                        self.add_project_dialog();
                    }
                    ui.separator();
                    if ui
                        .button("Refresh")
                        .on_hover_text("Rescan project folders")
                        .clicked()
                    {
                        self.refresh_projects();
                    }
                });
                ui.add_space(4.0);
                if self.sidebar_tab == SidebarTab::Contents {
                    self.outline_sidebar(ui);
                    return;
                }
                if self.sidebar_tab == SidebarTab::Search {
                    self.search_sidebar(ui);
                    return;
                }
                ui.add_sized(
                    [ui.available_width(), 34.0],
                    egui::TextEdit::singleline(&mut self.sidebar_search)
                        .hint_text("Filter PDFs")
                        .desired_width(f32::INFINITY)
                        .vertical_align(Align::Center)
                        .margin(Margin::symmetric(9, 6)),
                );
                ui.add_space(7.0);

                ScrollArea::vertical()
                    .id_salt("sidebar-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let mut remove_project = None;
                        let mut remove_recent = None;
                        let mut open_after_ui = None;
                        let mut open_with_request = None;
                        let mut rename_request = None;
                        let mut clear_file_alias = None;
                        let mut open_in_files = None;
                        let mut pin_file = None;
                        let mut unpin_file = None;
                        let mut pin_folder = None;
                        let mut unpin_folder = None;
                        if !self.state.pinned_folders.is_empty()
                            || !self.state.pinned_files.is_empty()
                        {
                            sidebar_section_header(ui, "PINNED", "", theme::MUTED);
                            for folder in self.state.pinned_folders.clone() {
                                if let Some(index) = self
                                    .state
                                    .projects
                                    .iter()
                                    .position(|project| project.root == folder)
                                {
                                    let project = self.state.projects[index].clone();
                                    let files = self
                                        .project_files
                                        .get(&project.root)
                                        .cloned()
                                        .unwrap_or_default();
                                    let matching_files: Vec<_> = files
                                        .into_iter()
                                        .filter(|path| {
                                            matches_search(
                                                path,
                                                &project.root,
                                                &self.sidebar_search,
                                            ) || self
                                                .state
                                                .file_aliases
                                                .get(path.as_path())
                                                .is_some_and(|alias| {
                                                    alias.to_lowercase().contains(
                                                        &self.sidebar_search.trim().to_lowercase(),
                                                    )
                                                })
                                        })
                                        .collect();
                                    let header = project_header(
                                        ui,
                                        &project.name,
                                        matching_files.len(),
                                        self.state.projects[index].expanded,
                                    );
                                    header
                                        .body
                                        .clone()
                                        .on_hover_text(project.root.display().to_string());
                                    if header.body.clicked() && !header.options.clicked() {
                                        self.state.projects[index].expanded =
                                            !self.state.projects[index].expanded;
                                        self.persist();
                                    }
                                    if let Some(action) = sidebar_options_popup(
                                        &header.options,
                                        false,
                                        Some("Remove from sidebar"),
                                        false,
                                        true,
                                    ) {
                                        match action {
                                            SidebarMenuAction::Rename => {
                                                rename_request = Some((
                                                    RenameTarget::Project(index),
                                                    project.name.clone(),
                                                ));
                                            }
                                            SidebarMenuAction::Remove => {
                                                remove_project = Some(index)
                                            }
                                            SidebarMenuAction::Unpin => {
                                                unpin_folder = Some(folder.clone())
                                            }
                                            SidebarMenuAction::OpenInFiles => {
                                                open_in_files = Some(folder.clone())
                                            }
                                            _ => {}
                                        }
                                    }
                                    if self.state.projects[index].expanded {
                                        let tree = build_pdf_tree(&project.root, &matching_files);
                                        if let Some(action) = render_pdf_tree(
                                            ui,
                                            &tree,
                                            0,
                                            self.current_path.as_deref(),
                                            &self.state.file_aliases,
                                            &mut self.collapsed_directories,
                                            &self.state.pinned_files,
                                            &self.state.pinned_folders,
                                            self.state.sidebar_file_icons,
                                        ) {
                                            match action {
                                                TreeAction::Open(path) => {
                                                    open_after_ui = Some(path)
                                                }
                                                TreeAction::OpenWith(path) => {
                                                    open_with_request = Some(path)
                                                }
                                                TreeAction::OpenInFiles(path) => {
                                                    open_in_files = Some(path)
                                                }
                                                TreeAction::Rename(path, name) => {
                                                    rename_request =
                                                        Some((RenameTarget::File(path), name));
                                                }
                                                TreeAction::ResetName(path) => {
                                                    clear_file_alias = Some(path)
                                                }
                                                TreeAction::PinFile(path) => pin_file = Some(path),
                                                TreeAction::UnpinFile(path) => {
                                                    unpin_file = Some(path)
                                                }
                                                TreeAction::PinFolder(path) => {
                                                    pin_folder = Some(path)
                                                }
                                                TreeAction::UnpinFolder(path) => {
                                                    unpin_folder = Some(path)
                                                }
                                            }
                                        }
                                    }
                                } else {
                                    let row = nav_row(
                                        ui,
                                        &file_name(&folder),
                                        false,
                                        NavIcon::Folder,
                                        0.0,
                                        self.state.sidebar_file_icons,
                                    );
                                    if row.body.clicked() && !row.options.clicked() {
                                        self.open_folder_browser(folder.clone());
                                    }
                                    if let Some(action) = sidebar_options_popup(
                                        &row.options,
                                        false,
                                        None,
                                        false,
                                        true,
                                    ) {
                                        match action {
                                            SidebarMenuAction::Unpin => unpin_folder = Some(folder),
                                            SidebarMenuAction::OpenInFiles => {
                                                open_in_files = Some(folder)
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                            for path in self.state.pinned_files.clone() {
                                let row = nav_row(
                                    ui,
                                    &self.state.display_name(&path),
                                    self.current_path.as_deref() == Some(path.as_path()),
                                    NavIcon::File,
                                    0.0,
                                    self.state.sidebar_file_icons,
                                );
                                if row.body.clicked() && !row.options.clicked() {
                                    open_after_ui = Some(path.clone());
                                }
                                if let Some(action) = sidebar_options_popup(
                                    &row.options,
                                    self.state.file_aliases.contains_key(&path),
                                    None,
                                    true,
                                    true,
                                ) {
                                    match action {
                                        SidebarMenuAction::OpenWith => {
                                            open_with_request = Some(path)
                                        }
                                        SidebarMenuAction::OpenInFiles => {
                                            open_in_files = Some(path)
                                        }
                                        SidebarMenuAction::Unpin => unpin_file = Some(path),
                                        SidebarMenuAction::Rename => {
                                            rename_request = Some((
                                                RenameTarget::File(path.clone()),
                                                self.state.display_name(&path),
                                            ))
                                        }
                                        SidebarMenuAction::ResetName => {
                                            clear_file_alias = Some(path)
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            ui.add_space(10.0);
                        }
                        let has_unpinned_projects = self
                            .state
                            .projects
                            .iter()
                            .any(|project| !self.state.pinned_folders.contains(&project.root));
                        if has_unpinned_projects || self.state.projects.is_empty() {
                            sidebar_section_header(ui, "PROJECTS", "", theme::MUTED);
                        }
                        if self.state.projects.is_empty() {
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(
                                    "Add a course or research folder. Only PDFs are shown.",
                                )
                                .color(theme::MUTED)
                                .size(13.0 * explorer_factor),
                            );
                        }

                        for index in 0..self.state.projects.len() {
                            let project = self.state.projects[index].clone();
                            if self.state.pinned_folders.contains(&project.root) {
                                continue;
                            }
                            let files = self
                                .project_files
                                .get(&project.root)
                                .cloned()
                                .unwrap_or_default();
                            let matching_files: Vec<_> = files
                                .into_iter()
                                .filter(|path| {
                                    matches_search(path, &project.root, &self.sidebar_search)
                                        || self.state.file_aliases.get(path.as_path()).is_some_and(
                                            |alias| {
                                                alias.to_lowercase().contains(
                                                    &self.sidebar_search.trim().to_lowercase(),
                                                )
                                            },
                                        )
                                })
                                .collect();

                            ui.add_space(3.0);
                            let header = project_header(
                                ui,
                                &project.name,
                                matching_files.len(),
                                self.state.projects[index].expanded,
                            );
                            header
                                .body
                                .clone()
                                .on_hover_text(project.root.display().to_string());
                            if header.body.clicked() && !header.options.clicked() {
                                self.state.projects[index].expanded =
                                    !self.state.projects[index].expanded;
                                self.persist();
                            }
                            if let Some(action) = sidebar_options_popup(
                                &header.options,
                                false,
                                Some("Remove from sidebar"),
                                false,
                                self.state.pinned_folders.contains(&project.root),
                            ) {
                                match action {
                                    SidebarMenuAction::OpenWith => {}
                                    SidebarMenuAction::Rename => {
                                        rename_request = Some((
                                            RenameTarget::Project(index),
                                            project.name.clone(),
                                        ));
                                    }
                                    SidebarMenuAction::Remove => remove_project = Some(index),
                                    SidebarMenuAction::ResetName => {}
                                    SidebarMenuAction::Pin => {
                                        pin_folder = Some(project.root.clone())
                                    }
                                    SidebarMenuAction::Unpin => {
                                        unpin_folder = Some(project.root.clone())
                                    }
                                    SidebarMenuAction::OpenInFiles => {
                                        open_in_files = Some(project.root.clone())
                                    }
                                }
                            }

                            if self.state.projects[index].expanded {
                                let tree = build_pdf_tree(&project.root, &matching_files);
                                if let Some(action) = render_pdf_tree(
                                    ui,
                                    &tree,
                                    0,
                                    self.current_path.as_deref(),
                                    &self.state.file_aliases,
                                    &mut self.collapsed_directories,
                                    &self.state.pinned_files,
                                    &self.state.pinned_folders,
                                    self.state.sidebar_file_icons,
                                ) {
                                    match action {
                                        TreeAction::Open(path) => open_after_ui = Some(path),
                                        TreeAction::OpenWith(path) => {
                                            open_with_request = Some(path)
                                        }
                                        TreeAction::OpenInFiles(path) => open_in_files = Some(path),
                                        TreeAction::Rename(path, name) => {
                                            rename_request = Some((RenameTarget::File(path), name));
                                        }
                                        TreeAction::ResetName(path) => {
                                            clear_file_alias = Some(path)
                                        }
                                        TreeAction::PinFile(path) => pin_file = Some(path),
                                        TreeAction::UnpinFile(path) => unpin_file = Some(path),
                                        TreeAction::PinFolder(path) => pin_folder = Some(path),
                                        TreeAction::UnpinFolder(path) => unpin_folder = Some(path),
                                    }
                                }
                            }
                        }

                        ui.add_space(18.0);
                        if collapsible_section_header(
                            ui,
                            "RECENTS",
                            self.session_recents.len().min(15),
                            self.state.recents_expanded,
                        )
                        .clicked()
                        {
                            self.state.recents_expanded = !self.state.recents_expanded;
                            self.persist();
                        }
                        let recent = self.session_recents.clone();
                        if self.state.recents_expanded {
                            for item in recent.iter().take(15) {
                                let selected =
                                    self.current_path.as_deref() == Some(item.path.as_path());
                                let row = nav_row(
                                    ui,
                                    &self.state.display_name(&item.path),
                                    selected,
                                    NavIcon::Recent,
                                    0.0,
                                    self.state.sidebar_file_icons,
                                );
                                if self.state.display_name(&item.path).chars().count()
                                    > max_sidebar_label_chars(
                                        &row.body,
                                        0.0,
                                        self.state.sidebar_file_icons,
                                    )
                                {
                                    row.body
                                        .clone()
                                        .on_hover_text(self.state.display_name(&item.path));
                                }
                                if row.body.clicked() && !row.options.clicked() {
                                    open_after_ui = Some(item.path.clone());
                                }
                                if let Some(action) = sidebar_options_popup(
                                    &row.options,
                                    self.state.file_aliases.contains_key(&item.path),
                                    Some("Remove from recents"),
                                    true,
                                    self.state.pinned_files.contains(&item.path),
                                ) {
                                    match action {
                                        SidebarMenuAction::OpenWith => {
                                            open_with_request = Some(item.path.clone());
                                        }
                                        SidebarMenuAction::OpenInFiles => {
                                            open_in_files = Some(item.path.clone())
                                        }
                                        SidebarMenuAction::Rename => {
                                            rename_request = Some((
                                                RenameTarget::File(item.path.clone()),
                                                self.state.display_name(&item.path),
                                            ));
                                        }
                                        SidebarMenuAction::ResetName => {
                                            clear_file_alias = Some(item.path.clone());
                                        }
                                        SidebarMenuAction::Remove => {
                                            remove_recent = Some(item.path.clone())
                                        }
                                        SidebarMenuAction::Pin => {
                                            pin_file = Some(item.path.clone())
                                        }
                                        SidebarMenuAction::Unpin => {
                                            unpin_file = Some(item.path.clone())
                                        }
                                    }
                                }
                            }
                        }

                        if let Some(index) = remove_project {
                            let project = self.state.projects.remove(index);
                            self.project_files.remove(&project.root);
                            self.state
                                .pinned_folders
                                .retain(|path| path != &project.root);
                            self.persist();
                        }
                        if let Some(path) = remove_recent {
                            self.session_recents
                                .retain(|document| document.path != path);
                            self.state.remove_recent(&path);
                            self.persist();
                        }
                        if let Some(path) = pin_file {
                            self.state.pinned_files.retain(|item| item != &path);
                            self.state.pinned_files.insert(0, path);
                            self.persist();
                        }
                        if let Some(path) = unpin_file {
                            self.state.pinned_files.retain(|item| item != &path);
                            self.persist();
                        }
                        if let Some(path) = pin_folder {
                            self.state.pinned_folders.retain(|item| item != &path);
                            self.state.pinned_folders.insert(0, path.clone());
                            if let Some(index) = self
                                .state
                                .projects
                                .iter()
                                .position(|project| project.root == path)
                            {
                                let project = self.state.projects.remove(index);
                                self.state.projects.insert(0, project);
                            }
                            self.persist();
                        }
                        if let Some(path) = unpin_folder {
                            self.state.pinned_folders.retain(|item| item != &path);
                            self.persist();
                        }
                        if let Some(path) = clear_file_alias {
                            self.state.clear_file_alias(&path);
                            self.persist();
                        }
                        if let Some((target, current_name)) = rename_request {
                            self.rename_target = Some(target);
                            self.rename_buffer = current_name;
                            self.rename_focus_requested = true;
                        }
                        if let Some(path) = open_after_ui {
                            self.open_document(path);
                        }
                        if let Some(path) = open_with_request {
                            self.request_open_with(path);
                        }
                        if let Some(path) = open_in_files {
                            self.open_path_in_file_manager(&path);
                        }
                    });
            });
    }

    fn outline_sidebar(&mut self, ui: &mut egui::Ui) {
        let scale = sidebar_visual_scale(ui);
        let title = self
            .current_path
            .as_deref()
            .map(|path| self.state.display_name(path))
            .unwrap_or_else(|| "No PDF open".to_owned());
        ui.label(RichText::new(title).size(13.0 * scale).strong());
        ui.add_space(3.0);
        if self.current_path.is_none() {
            ui.label(RichText::new("Open a PDF to view its sections.").color(theme::MUTED));
            return;
        }
        if self.outline.is_empty() {
            ui.label(
                RichText::new("This PDF does not contain a bookmark outline.")
                    .size(12.5 * scale)
                    .color(theme::MUTED),
            );
            return;
        }
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{} navigable sections", self.outline.len()))
                    .size(11.0 * scale)
                    .color(theme::MUTED),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let all_collapsed = self
                    .outline
                    .iter()
                    .enumerate()
                    .filter(|(index, item)| {
                        self.outline
                            .get(index + 1)
                            .is_some_and(|next| next.depth > item.depth)
                    })
                    .all(|(index, _)| self.collapsed_outline_items.contains(&index));
                if ui
                    .small_button(if all_collapsed {
                        "Expand all"
                    } else {
                        "Collapse all"
                    })
                    .clicked()
                {
                    if all_collapsed {
                        self.collapsed_outline_items.clear();
                    } else {
                        self.collapsed_outline_items = self
                            .outline
                            .iter()
                            .enumerate()
                            .filter(|(index, item)| {
                                self.outline
                                    .get(index + 1)
                                    .is_some_and(|next| next.depth > item.depth)
                            })
                            .map(|(index, _)| index)
                            .collect();
                    }
                }
            });
        });
        ui.add_space(9.0);
        let items = self.outline.clone();
        let mut activated = None;
        ScrollArea::vertical()
            .id_salt("outline-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut hidden_below_depth = None;
                for (index, item) in items.iter().enumerate() {
                    if let Some(depth) = hidden_below_depth {
                        if item.depth > depth {
                            continue;
                        }
                        hidden_below_depth = None;
                    }
                    let has_children = items
                        .get(index + 1)
                        .is_some_and(|next| next.depth > item.depth);
                    let collapsed = self.collapsed_outline_items.contains(&index);
                    let indent = item.depth as f32 * 14.0;
                    let (rect, response) = ui.allocate_exact_size(
                        vec2(
                            ui.available_width(),
                            if item.depth == 0 { 34.0 } else { 30.0 },
                        ),
                        Sense::click(),
                    );
                    if response.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(6),
                            Color32::from_rgb(31, 34, 41),
                        );
                    }
                    let color = if item.target.is_some() {
                        Color32::from_rgb(218, 220, 226)
                    } else {
                        theme::MUTED
                    };
                    let max_chars = ((rect.width() - indent - 42.0) / 6.9).max(6.0) as usize;
                    if has_children {
                        let chevron_center = pos2(rect.left() + 10.0 + indent, rect.center().y);
                        let points = if collapsed {
                            vec![
                                chevron_center + vec2(-2.0, -4.0),
                                chevron_center + vec2(2.5, 0.0),
                                chevron_center + vec2(-2.0, 4.0),
                            ]
                        } else {
                            vec![
                                chevron_center + vec2(-4.0, -2.0),
                                chevron_center + vec2(0.0, 2.5),
                                chevron_center + vec2(4.0, -2.0),
                            ]
                        };
                        ui.painter()
                            .add(egui::Shape::line(points, Stroke::new(1.25, theme::MUTED)));
                    }
                    ui.painter().text(
                        pos2(rect.left() + 22.0 + indent, rect.center().y),
                        Align2::LEFT_CENTER,
                        elide(&item.title, max_chars),
                        FontId::proportional(if item.depth == 0 { 13.5 } else { 12.8 }),
                        color,
                    );
                    if let Some(LinkTarget::Page(page)) = item.target {
                        ui.painter().text(
                            pos2(rect.right() - 6.0, rect.center().y),
                            Align2::RIGHT_CENTER,
                            (page + 1).to_string(),
                            FontId::proportional(10.5),
                            theme::MUTED,
                        );
                    }
                    response.clone().on_hover_text(&item.title);
                    if response.clicked() {
                        let pointer = response.interact_pointer_pos();
                        let clicked_disclosure = has_children
                            && pointer
                                .is_some_and(|pointer| pointer.x < rect.left() + indent + 21.0);
                        if clicked_disclosure {
                            if collapsed {
                                self.collapsed_outline_items.remove(&index);
                            } else {
                                self.collapsed_outline_items.insert(index);
                            }
                        } else if item.target.is_some() {
                            activated = item.target.clone();
                        }
                    }
                    if has_children && collapsed {
                        hidden_below_depth = Some(item.depth);
                    }
                }
            });
        if let Some(target) = activated {
            self.activate_link(target, ui.ctx());
        }
    }

    fn activate_link(&mut self, target: LinkTarget, ctx: &egui::Context) {
        match target {
            LinkTarget::Uri(uri) if is_safe_external_uri(&uri) => {
                ctx.open_url(egui::OpenUrl::new_tab(&uri));
                self.status = format!("Opening {uri}");
            }
            LinkTarget::Uri(_) => {
                self.status = "Blocked an unsupported link type.".to_owned();
            }
            LinkTarget::Page(page) => {
                let page = page.min(self.page_sizes.len().saturating_sub(1));
                self.jump_to_page = Some(page);
                self.status = format!("Jumped to page {}.", page + 1);
                ctx.request_repaint();
            }
        }
    }

    fn document_tabs(&mut self, ui: &mut egui::Ui) {
        let documents = self.state.open_documents.clone();
        let mut activate = None;
        let mut close = None;
        ScrollArea::horizontal()
            .id_salt("document-tabs")
            .auto_shrink([false, true])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for path in &documents {
                        let active = self.current_path.as_deref() == Some(path.as_path());
                        Frame::new()
                            .fill(if active {
                                Color32::from_rgb(37, 37, 37)
                            } else {
                                Color32::TRANSPARENT
                            })
                            .corner_radius(CornerRadius::same(6))
                            .inner_margin(Margin::symmetric(6, 1))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                RichText::new(elide(
                                                    &self.state.display_name(path),
                                                    28,
                                                ))
                                                .size(11.8),
                                            )
                                            .frame(false),
                                        )
                                        .on_hover_text(path.display().to_string())
                                        .clicked()
                                    {
                                        activate = Some(path.clone());
                                    }
                                    if ui
                                        .add(
                                            egui::Button::new(RichText::new("×").size(12.5))
                                                .frame(false),
                                        )
                                        .on_hover_text("Close tab")
                                        .clicked()
                                    {
                                        close = Some(path.clone());
                                    }
                                });
                            });
                    }
                    if ui
                        .add(egui::Button::new(RichText::new("+").size(18.0)).frame(false))
                        .on_hover_text("Open PDF")
                        .clicked()
                    {
                        self.open_file_dialog();
                    }
                });
            });
        if let Some(path) = close {
            self.close_document_tab(&path);
        } else if let Some(path) = activate {
            self.open_document(path);
        }
    }

    fn search_sidebar(&mut self, ui: &mut egui::Ui) {
        let scale = sidebar_visual_scale(ui);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("SEARCH")
                    .size(11.5 * scale)
                    .strong()
                    .color(theme::MUTED),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(egui::Button::new(RichText::new("×").size(16.0 * scale)).frame(false))
                    .on_hover_text("Close search")
                    .clicked()
                {
                    self.sidebar_tab = SidebarTab::Library;
                    self.show_search = false;
                }
            });
        });
        ui.add_space(6.0);
        let response = ui.add_sized(
            [ui.available_width(), 32.0],
            egui::TextEdit::singleline(&mut self.search_query)
                .hint_text("Search PDFs")
                .margin(Margin::symmetric(8, 6)),
        );
        if response.changed() {
            self.run_search(ui.ctx());
        }
        // Enter remains a convenient explicit re-run, while each edit searches live.
        if response.has_focus()
            && ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Enter))
        {
            self.run_search(ui.ctx());
        }
        ui.horizontal(|ui| {
            let has_navigation = self.search_navigation_len() > 0;
            if ui
                .add_enabled(has_navigation, egui::Button::new("Previous"))
                .clicked()
            {
                self.step_search(-1);
            }
            if ui
                .add_enabled(has_navigation, egui::Button::new("Next"))
                .clicked()
            {
                self.step_search(1);
            }
        });
        ui.add_space(10.0);
        ScrollArea::vertical()
            .id_salt("search-results-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| self.search_results(ui));
    }

    fn search_results(&mut self, ui: &mut egui::Ui) {
        let scale = sidebar_visual_scale(ui);
        if self.search_query.trim().is_empty() {
            ui.label(
                RichText::new(
                    "Results update as you type: current file, open tabs, and the current project.",
                )
                .size(12.0 * scale)
                .color(theme::MUTED),
            );
            return;
        }

        if collapsible_section_header(
            ui,
            "CURRENT FILE",
            self.search_matches.len(),
            self.search_current_expanded,
        )
        .clicked()
        {
            self.search_current_expanded = !self.search_current_expanded;
        }
        if self.search_current_expanded {
            if self.search_matches.is_empty() {
                ui.label(
                    RichText::new("No matches in the open PDF.")
                        .size(12.0 * scale)
                        .color(theme::MUTED),
                );
            } else {
                let matches = self.search_matches.clone();
                for (index, result) in matches.iter().enumerate() {
                    let response = search_result_row(
                        ui,
                        &format!("Page {}", result.page_index + 1),
                        &result.excerpt,
                        index == self.search_index,
                    );
                    if response.clicked() {
                        self.search_index = index;
                        self.jump_to_page = Some(result.page_index);
                    }
                }
            }
        }

        ui.add_space(4.0);
        let tab_result_count = self
            .tab_search_matches
            .iter()
            .map(|result| result.matches.len())
            .sum::<usize>();
        if collapsible_section_header(ui, "OPEN TABS", tab_result_count, self.search_tabs_expanded)
            .clicked()
        {
            self.search_tabs_expanded = !self.search_tabs_expanded;
        }
        if self.search_tabs_expanded && self.tab_search_scan.is_some() {
            ui.label(
                RichText::new("Searching open tabs...")
                    .size(12.0 * scale)
                    .color(theme::MUTED),
            );
        } else if self.search_tabs_expanded && self.tab_search_matches.is_empty() {
            ui.label(
                RichText::new("No matches in open tabs.")
                    .size(12.0 * scale)
                    .color(theme::MUTED),
            );
        } else if self.search_tabs_expanded {
            let matches = self.tab_search_matches.clone();
            let mut activate = None;
            for file in &matches {
                for result in &file.matches {
                    let response = search_result_row(
                        ui,
                        &format!(
                            "{} · p. {}",
                            elide(&file_name(&file.path), 20),
                            result.page_index + 1
                        ),
                        &result.excerpt,
                        false,
                    );
                    if response.clicked() {
                        activate = Some((file.path.clone(), result.page_index));
                    }
                }
            }
            if let Some((path, page)) = activate {
                self.open_document(path);
                self.jump_to_page = Some(page);
            }
        }

        ui.add_space(4.0);
        let project_result_count = self
            .workspace_search_matches
            .iter()
            .map(|result| result.matches.len())
            .sum::<usize>();
        if collapsible_section_header(
            ui,
            "CURRENT PROJECT",
            project_result_count,
            self.search_project_expanded,
        )
        .clicked()
        {
            self.search_project_expanded = !self.search_project_expanded;
        }
        if self.search_project_expanded && self.workspace_search_scan.is_some() {
            ui.label(
                RichText::new("Searching project PDFs...")
                    .size(12.0 * scale)
                    .color(theme::MUTED),
            );
        } else if self.search_project_expanded && self.workspace_search_matches.is_empty() {
            ui.label(
                RichText::new("No matches in the current project.")
                    .size(12.0 * scale)
                    .color(theme::MUTED),
            );
        } else if self.search_project_expanded {
            let matches = self.workspace_search_matches.clone();
            let mut activate = None;
            for file in &matches {
                for result in &file.matches {
                    let response = search_result_row(
                        ui,
                        &format!(
                            "{} · p. {}",
                            elide(&file_name(&file.path), 20),
                            result.page_index + 1
                        ),
                        &result.excerpt,
                        false,
                    );
                    if response.clicked() {
                        activate = Some((file.path.clone(), result.page_index));
                    }
                }
            }
            if let Some((path, page)) = activate {
                self.open_document(path);
                self.jump_to_page = Some(page);
            }
        }
    }

    fn toolbar(&mut self, root: &mut egui::Ui) {
        if !self.toolbar_visible || self.reader_mode || self.presentation_mode {
            return;
        }
        egui::Panel::top("toolbar")
            .frame(
                Frame::new()
                    .fill(theme::PANEL_BG)
                    .inner_margin(Margin::symmetric(8, 1)),
            )
            .show(root, |ui| {
                ui.add_space(5.0);
                self.document_tabs(ui);
                ui.separator();
                let mut breadcrumb_action = None;
                let breadcrumb_path = self
                    .browser_directory
                    .as_deref()
                    .or(self.current_path.as_deref())
                    .map(Path::to_path_buf);
                ScrollArea::horizontal()
                    .id_salt("path-actions-scroll")
                    .auto_shrink([false, true])
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .scroll_source(egui::scroll_area::ScrollSource::ALL)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            breadcrumb_action = path_breadcrumbs(ui, breadcrumb_path.as_deref());
                            if icon_button(
                                ui,
                                ToolbarIcon::Folder,
                                false,
                                breadcrumb_path.is_some(),
                                "Open this directory in Files",
                            )
                            .clicked()
                                && breadcrumb_path.is_some()
                            {
                                self.open_current_directory();
                            }
                            if self.current_path.is_some() {
                                ui.add_space(8.0);
                                ui.separator();
                                if icon_button(
                                    ui,
                                    ToolbarIcon::Export,
                                    false,
                                    true,
                                    "Export annotated copy",
                                )
                                .clicked()
                                {
                                    self.export_annotated_copy();
                                }
                                if icon_button(
                                    ui,
                                    ToolbarIcon::Save,
                                    false,
                                    !self.current_annotations().is_empty(),
                                    "Save annotations into this PDF",
                                )
                                .clicked()
                                    && !self.current_annotations().is_empty()
                                {
                                    self.confirm_save_to_source = true;
                                }
                                if icon_button(
                                    ui,
                                    ToolbarIcon::Print,
                                    false,
                                    true,
                                    "Print (Ctrl+P)",
                                )
                                .clicked()
                                {
                                    self.request_print();
                                }
                                if icon_button(
                                    ui,
                                    ToolbarIcon::OpenWith,
                                    false,
                                    true,
                                    "Open with another app",
                                )
                                .clicked()
                                    && let Some(path) = self.current_path.clone()
                                {
                                    self.request_open_with(path);
                                }
                            }
                        });
                    });
                match breadcrumb_action {
                    Some(BreadcrumbAction::Browse(path)) => self.open_folder_browser(path),
                    Some(BreadcrumbAction::Copy(path)) => {
                        ui.ctx().copy_text(path.display().to_string());
                        self.status = "Path copied.".to_owned();
                    }
                    None => {}
                }
                if self.current_path.is_some() {
                    ui.separator();
                    ScrollArea::horizontal()
                        .id_salt("viewer-tools-scroll")
                        .auto_shrink([false, true])
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                        .scroll_source(egui::scroll_area::ScrollSource::ALL)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                // Zoom group: document zoom is primary; surrounding chrome follows the configured ratio.
                                if compact_text_button(ui, "−", "Zoom out (Ctrl+-)").clicked() {
                                    let previous = self.state.ui_scale;
                                    self.state.ui_scale = (self.state.ui_scale - 0.1).max(0.7);
                                    self.zoom *= self.state.ui_scale / previous.max(0.1);
                                    ui.ctx().set_zoom_factor(chrome_zoom(
                                        self.state.ui_scale,
                                        self.state.chrome_scale_ratio,
                                    ));
                                    self.persist();
                                }
                                ui.label(
                                    RichText::new(format!(
                                        "{}%",
                                        (self.state.ui_scale * 100.0).round() as i32
                                    ))
                                    .monospace(),
                                );
                                if compact_text_button(ui, "+", "Zoom in (Ctrl++)").clicked() {
                                    let previous = self.state.ui_scale;
                                    self.state.ui_scale = (self.state.ui_scale + 0.1).min(5.0);
                                    self.zoom *= self.state.ui_scale / previous.max(0.1);
                                    ui.ctx().set_zoom_factor(chrome_zoom(
                                        self.state.ui_scale,
                                        self.state.chrome_scale_ratio,
                                    ));
                                    self.persist();
                                }
                                ui.add_space(5.0);
                                ui.separator();
                                ui.add_space(5.0);
                                if !self.page_sizes.is_empty() {
                                    if !self.page_entry_editing {
                                        self.page_entry = (self.current_page + 1).to_string();
                                    }
                                    let response = ui.add_sized(
                                        [42.0, 20.0],
                                        egui::TextEdit::singleline(&mut self.page_entry)
                                            .horizontal_align(Align::Center)
                                            .margin(Margin::symmetric(4, 2)),
                                    );
                                    self.page_entry_editing = response.has_focus();
                                    if response.changed()
                                        && let Ok(page) = self.page_entry.trim().parse::<usize>()
                                        && (1..=self.page_sizes.len()).contains(&page)
                                    {
                                        self.jump_to_page = Some(page - 1);
                                    }
                                    if response.lost_focus()
                                        && !response.has_focus()
                                        && !self.page_entry.trim().parse::<usize>().is_ok_and(
                                            |page| (1..=self.page_sizes.len()).contains(&page),
                                        )
                                    {
                                        self.page_entry = (self.current_page + 1).to_string();
                                    }
                                    ui.label(
                                        RichText::new(format!("of {}", self.page_sizes.len()))
                                            .color(theme::MUTED),
                                    );
                                }
                                ui.add_space(5.0);
                                ui.separator();
                                ui.add_space(5.0);
                                tool_icon_button(
                                    ui,
                                    &mut self.tool_mode,
                                    ToolMode::Highlight,
                                    ToolbarIcon::Highlight,
                                    "Highlight text",
                                );
                                tool_icon_button(
                                    ui,
                                    &mut self.tool_mode,
                                    ToolMode::Pen,
                                    ToolbarIcon::Pen,
                                    "Draw",
                                );
                                tool_icon_button(
                                    ui,
                                    &mut self.tool_mode,
                                    ToolMode::Eraser,
                                    ToolbarIcon::Eraser,
                                    "Erase annotations",
                                );
                                tool_icon_button(
                                    ui,
                                    &mut self.tool_mode,
                                    ToolMode::Note,
                                    ToolbarIcon::Comment,
                                    "Add comment",
                                );
                                ui.menu_button("···", |ui| {
                                    tool_button(
                                        ui,
                                        &mut self.tool_mode,
                                        ToolMode::TextBox,
                                        "Text box",
                                        "Place editable text directly on the page",
                                    );
                                    tool_button(
                                        ui,
                                        &mut self.tool_mode,
                                        ToolMode::Rectangle,
                                        "Rectangle",
                                        "Drag a rectangle",
                                    );
                                    tool_button(
                                        ui,
                                        &mut self.tool_mode,
                                        ToolMode::Ellipse,
                                        "Ellipse",
                                        "Drag an ellipse",
                                    );
                                    tool_button(
                                        ui,
                                        &mut self.tool_mode,
                                        ToolMode::Signature,
                                        "Signature",
                                        "Draw a signature with a black pen",
                                    );
                                    tool_button(
                                        ui,
                                        &mut self.tool_mode,
                                        ToolMode::Stamp,
                                        "Stamp",
                                        "Place a custom text stamp",
                                    );
                                    tool_button(
                                        ui,
                                        &mut self.tool_mode,
                                        ToolMode::Attachment,
                                        "Attach file",
                                        "Attach a local file reference to the page",
                                    );
                                });
                                tool_icon_button(
                                    ui,
                                    &mut self.tool_mode,
                                    ToolMode::Ocr,
                                    ToolbarIcon::Ocr,
                                    "Snip OCR",
                                );
                                tool_icon_button(
                                    ui,
                                    &mut self.tool_mode,
                                    ToolMode::MathOcr,
                                    ToolbarIcon::Math,
                                    "Math OCR (experimental)",
                                );
                                ui.separator();
                                if icon_button(
                                    ui,
                                    ToolbarIcon::Undo,
                                    false,
                                    !self.annotation_undo.is_empty(),
                                    "Undo",
                                )
                                .clicked()
                                {
                                    self.undo_annotation();
                                }
                                if icon_button(
                                    ui,
                                    ToolbarIcon::Redo,
                                    false,
                                    !self.annotation_redo.is_empty(),
                                    "Redo",
                                )
                                .clicked()
                                {
                                    self.redo_annotation();
                                }
                                ui.separator();
                                ui.scope(|ui| {
                                    ui.spacing_mut().interact_size = vec2(22.0, 22.0);
                                    ui.color_edit_button_srgba(&mut self.annotation_color);
                                });
                                ui.separator();
                                if icon_button(
                                    ui,
                                    ToolbarIcon::Reader,
                                    false,
                                    true,
                                    "Reader mode — hide the sidebar and toolbar",
                                )
                                .clicked()
                                {
                                    self.set_reader_mode(ui.ctx(), true);
                                }
                                if icon_button(
                                    ui,
                                    ToolbarIcon::Present,
                                    false,
                                    true,
                                    "Presentation mode — full screen",
                                )
                                .clicked()
                                {
                                    self.set_presentation_mode(ui.ctx(), true);
                                }
                                if icon_button(
                                    ui,
                                    ToolbarIcon::CollapseToolbar,
                                    false,
                                    true,
                                    "Hide toolbar",
                                )
                                .clicked()
                                {
                                    self.toolbar_visible = false;
                                }
                            });
                        });
                }
            });
    }

    fn viewer(&mut self, root: &mut egui::Ui) {
        let ctx = root.ctx().clone();
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(theme::CANVAS_BG)
                    .inner_margin(Margin::ZERO),
            )
            .show(root, |ui| {
                if self.browser_directory.is_some() {
                    self.folder_browser(ui);
                    return;
                }
                if self.current_path.is_none() {
                    self.empty_state(ui);
                    return;
                }
                if self.page_sizes.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.spinner();
                        ui.label(&self.status);
                    });
                    return;
                }

                let viewer_rect = ui.clip_rect();
                let zoom_before_pinch = self.zoom;
                let (pointer_position, egui_pinch_zoom, time) = ctx.input(|input| {
                    (
                        input.pointer.hover_pos().or_else(|| input.pointer.interact_pos()),
                        trackpad_pinch_factor(&input.events),
                        input.time,
                    )
                });
                // Both winit/egui and XInput can report the *same* physical
                // gesture on X11. Native gesture Begin/End makes XInput the
                // authoritative source during the pinch, including a short
                // grace period for late synthetic Ctrl+wheel events.
                let mut native_factor = 1.0_f32;
                let mut native_event_seen = false;
                for event in self.pending_native_pinches.drain(..) {
                    native_event_seen = true;
                    match event {
                        crate::gestures::NativePinchEvent::Begin => {
                            self.native_pinch_active = true;
                        }
                        crate::gestures::NativePinchEvent::Update(factor) => {
                            self.native_pinch_active = true;
                            native_factor *= factor;
                        }
                        crate::gestures::NativePinchEvent::End => {
                            self.native_pinch_active = false;
                        }
                    }
                }
                if native_event_seen {
                    self.native_pinch_suppress_until = time + 0.20;
                } else if self.native_pinch_active && time > self.native_pinch_suppress_until + 0.30 {
                    // Recover if an X11 compositor never delivers PinchEnd.
                    self.native_pinch_active = false;
                }
                let use_native = native_event_seen
                    || self.native_pinch_active
                    || time < self.native_pinch_suppress_until;
                let pinch_zoom = if use_native { native_factor } else { egui_pinch_zoom };
                let over_viewer = pointer_position.is_none_or(|pos| viewer_rect.contains(pos));
                if over_viewer && (pinch_zoom - 1.0).abs() > 0.0001 {
                    // Eguis ScrollArea also consumes scroll deltas. A gesture
                    // must never both scale the page and scroll the document.
                    self.apply_pinch_zoom(pinch_zoom, Some(pointer_position.unwrap_or(viewer_rect.center())), &ctx);
                }
                let pinch_in_progress = over_viewer
                    && (use_native || (egui_pinch_zoom - 1.0).abs() > 0.0001);
                if pinch_in_progress {
                    // Native touchpads can deliver a pinch and a synthetic wheel
                    // event on separate paths. Discard the smooth-wheel movement;
                    // the document ScrollArea also has its wheel source disabled
                    // for this frame below.
                    ctx.input_mut(|input| input.smooth_scroll_delta = egui::Vec2::ZERO);
                }
                if pinch_in_progress && std::env::var_os("SNAPPDF_PINCH_DEBUG").is_some() {
                    eprintln!(
                        "[snappdf pinch] source={} native={:.5} egui={:.5} zoom={:.4} old_offset={:?}",
                        if use_native { "xinput" } else { "egui" },
                        native_factor, egui_pinch_zoom, self.zoom, self.document_scroll_offset,
                    );
                }
                if self.pending_selection_highlight && self.selection_text_ready() {
                    self.highlight_text_selection();
                }

                let available_page_width = (ui.available_width() - 56.0).max(100.0);
                if self.fit_width_requested {
                    let first_width = self.page_sizes[0][0].max(1.0);
                    self.zoom = (available_page_width / first_width).clamp(0.4, 3.0);
                    self.fit_width_requested = false;
                }
                if self.presentation_fit_height_requested {
                    if self.presentation_fit_delay_frames > 0 {
                        self.presentation_fit_delay_frames -= 1;
                        ctx.request_repaint();
                    } else if let Some(size) = self.page_sizes.get(self.current_page) {
                        // Presentation has no chrome: every newly shown page is
                        // independently fit to the available screen height.
                        self.zoom = ((ui.available_height() - 2.0) / size[1].max(1.0))
                            .clamp(0.2, 8.0);
                        self.presentation_fit_height_requested = false;
                    }
                }

                let pixels_per_point = ctx.pixels_per_point();
                let page_sizes = self.page_sizes.clone();
                let generation = self.generation;
                let zoom = self.zoom;
                let frame_number = self.frame_number;
                let mut visible_page = self.current_page;
                let page_gap = if self.presentation_mode {
                    0.0
                } else if self.reader_mode {
                    6.0
                } else {
                    PAGE_GAP
                };
                let presentation_page = self.current_page;
                // Keep a real horizontal canvas when a pinched page is wider than
                // the viewport. `ScrollArea::both` then maps a two-finger slide to
                // natural vertical and horizontal panning instead of clipping it.
                let canvas_width = page_sizes
                    .iter()
                    .map(|size| size[0] * zoom + 56.0)
                    .fold(ui.available_width(), f32::max);

                // Solve the new scroll offset *before* egui lays out the pages.
                // A queued scroll_with_delta() adjusts a later frame and causes
                // oscillation when gesture updates arrive every frame.
                let anchored_offset = self.pinch_anchor.take().map(|anchor| {
                    let previous_canvas_width = page_sizes
                        .iter()
                        .map(|size| size[0] * zoom_before_pinch + 56.0)
                        .fold(ui.available_width(), f32::max);
                    pinch_anchored_offset(
                        self.document_scroll_offset,
                        anchor,
                        &page_sizes,
                        zoom_before_pinch,
                        zoom,
                        previous_canvas_width,
                        canvas_width,
                        self.presentation_mode,
                    )
                });

                let scroll_source = egui::containers::scroll_area::ScrollSource {
                    mouse_wheel: !pinch_in_progress,
                    ..Default::default()
                };
                let mut document_scroll = ScrollArea::both()
                    .id_salt("document-scroll")
                    .auto_shrink([false, false])
                    .animated(false)
                    .scroll_source(scroll_source)
                    .wheel_scroll_multiplier(vec2(
                        WHEEL_SCROLL_MULTIPLIER,
                        WHEEL_SCROLL_MULTIPLIER,
                    ));
                if let Some(offset) = anchored_offset {
                    document_scroll = document_scroll.scroll_offset(offset);
                }
                let scroll_output = document_scroll
                    .show_viewport(ui, |ui, _viewport| {
                        ui.set_min_width(canvas_width);
                        if self.keyboard_scroll != 0.0 {
                            ui.scroll_with_delta(vec2(0.0, self.keyboard_scroll));
                            self.keyboard_scroll = 0.0;
                        }
                        ui.add_space(page_gap);
                        let mut laid_out_pages = Vec::with_capacity(page_sizes.len());
                        for (index, source_size) in page_sizes.iter().enumerate() {
                            if self.presentation_mode && index != presentation_page {
                                continue;
                            }
                            let display_size = vec2(source_size[0] * zoom, source_size[1] * zoom);
                            let row_width = canvas_width;
                            let (row_rect, _) = ui.allocate_exact_size(
                                vec2(row_width, display_size.y + page_gap),
                                Sense::hover(),
                            );
                            let page_rect = Rect::from_min_size(
                                pos2(row_rect.center().x - display_size.x / 2.0, row_rect.min.y),
                                display_size,
                            );
                            laid_out_pages.push((index, page_rect));
                        }
                        self.page_rects = laid_out_pages.clone();
                        ui.add_space(page_gap);

                        let mut jumped_to_page = false;
                        if let Some(target) = self.jump_to_page.take()
                            && let Some((_, rect)) =
                                laid_out_pages.iter().find(|(index, _)| *index == target)
                        {
                            ui.scroll_to_rect_animation(
                                *rect,
                                Some(Align::Min),
                                egui::style::ScrollAnimation::none(),
                            );
                            visible_page = target;
                            jumped_to_page = true;
                        }

                        let viewport = ui.clip_rect();
                        if !jumped_to_page
                            && let Some((index, _)) =
                                laid_out_pages.iter().min_by(|(_, left), (_, right)| {
                                    (left.center().y - viewport.center().y)
                                        .abs()
                                        .total_cmp(&(right.center().y - viewport.center().y).abs())
                                })
                        {
                            visible_page = *index;
                        }
                        self.worker.focus_page(visible_page);
                        self.pending_pages.retain(|pending| {
                            pending.generation == generation
                                && pending.index.abs_diff(visible_page) <= 2
                        });
                        let selected_pages = self.text_selection.map(|selection| {
                            let (first, last) = selection.ordered();
                            (first.page, last.page)
                        });
                        self.pending_text.retain(|(index, pending_generation)| {
                            *pending_generation == generation
                                && (index.abs_diff(visible_page) <= 2
                                    || selected_pages.is_some_and(|(first, last)|
                                        (first..=last).contains(index)))
                        });

                        // Selection is a document-level gesture, not a gesture of
                        // the page on which the mouse was pressed. Otherwise the
                        // drag stops tracking when that page leaves the viewport.
                        if self.text_selection_dragging {
                            let (pointer, down) = ctx.input(|input| {
                                (input.pointer.hover_pos().or_else(|| input.pointer.interact_pos()),
                                 input.pointer.primary_down())
                            });
                            if let Some(pointer) = pointer {
                                let edge_delta = selection_edge_scroll(pointer.y, viewport);
                                if down && edge_delta.abs() > 0.1 {
                                    ui.scroll_with_delta_animation(
                                        vec2(0.0, edge_delta),
                                        egui::style::ScrollAnimation::none(),
                                    );
                                    ctx.request_repaint();
                                }
                                let clamped = pos2(
                                    pointer.x.clamp(viewport.left(), viewport.right()),
                                    pointer.y.clamp(viewport.top(), viewport.bottom()),
                                );
                                if let Some((page, rect)) = laid_out_pages.iter().min_by(|(_, a), (_, b)| {
                                    let distance = |rect: &Rect| {
                                        let dx = (rect.left() - clamped.x).max(0.0)
                                            + (clamped.x - rect.right()).max(0.0);
                                        let dy = (rect.top() - clamped.y).max(0.0)
                                            + (clamped.y - rect.bottom()).max(0.0);
                                        dx * dx + dy * dy
                                    };
                                    distance(a).total_cmp(&distance(b))
                                }) {
                                    // Already-visible layers update immediately; newly
                                    // encountered pages can be extracted asynchronously.
                                    if let Some(glyphs) = self.text_layers.get(page) {
                                        if let Some(glyph) = nearest_drag_glyph(
                                            glyphs, *rect, page_sizes[*page], clamped,
                                        ) && let Some(selection) = self.text_selection.as_mut() {
                                            selection.focus = TextPosition { page: *page, glyph };
                                        }
                                    } else {
                                        self.request_text(*page, generation);
                                    }
                                }
                            }
                            if !down {
                                self.text_selection_dragging = false;
                                if self.tool_mode == ToolMode::Highlight {
                                    self.highlight_text_selection();
                                }
                            }
                        }
                        if let Some(selection) = self.text_selection {
                            let (start, end) = selection.ordered();
                            // Include intervening pages, even when a fast drag
                            // crosses one before its text layer has loaded.
                            for page in start.page..=end.page {
                                self.request_text(page, generation);
                            }
                        }

                        let preload = viewport.expand2(vec2(0.0, viewport.height() * 1.25));
                        let mut selection_action_rect = None;
                        for (index, page_rect) in laid_out_pages {
                            if page_rect.intersects(preload) {
                                let target_width =
                                    quantized_render_width(page_rect.width() * pixels_per_point);
                                let key = PageKey {
                                    index,
                                    width: target_width,
                                    generation,
                                };

                                ui.painter().rect_filled(
                                    page_rect.translate(vec2(4.0, 5.0)),
                                    2.0,
                                    Color32::from_black_alpha(90),
                                );
                                ui.painter()
                                    .rect_filled(page_rect, 1.0, Color32::from_gray(242));

                                let mut needs_render = true;
                                if let Some(page) = self.textures.get_mut(&index)
                                    && page.generation == generation
                                {
                                    page.last_used_frame = frame_number;
                                    ui.painter().image(
                                        page.texture.id(),
                                        page_rect,
                                        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                                        Color32::WHITE,
                                    );
                                    needs_render =
                                        (page.rendered_width as f32) < target_width as f32 * 0.92;
                                } else {
                                    ui.painter().text(
                                        page_rect.center(),
                                        Align2::CENTER_CENTER,
                                        format!("Rendering page {}", index + 1),
                                        FontId::proportional(14.0),
                                        Color32::from_gray(80),
                                    );
                                }
                                ui.painter().rect_stroke(
                                    page_rect,
                                    1.0,
                                    Stroke::new(1.0, Color32::from_gray(80)),
                                    StrokeKind::Inside,
                                );
                                if needs_render {
                                    self.request_page(key);
                                }
                                self.request_text(index, generation);
                                if self.show_search
                                    && !self.search_query.trim().is_empty()
                                    && let Some(glyphs) = self.text_layers.get(&index)
                                {
                                    paint_search_hits(
                                        ui.painter(),
                                        glyphs,
                                        &self.search_query,
                                        page_rect,
                                        page_sizes[index],
                                    );
                                }

                                let page_annotations = self.current_annotations()
                                    .iter()
                                    .filter(|annotation| annotation.page == index)
                                    .cloned()
                                    .collect::<Vec<_>>();
                                paint_annotations(ui.painter(), &page_annotations, page_rect, page_sizes[index]);
                                if let Some(selection) = self.text_selection
                                    && let Some(glyphs) = self.text_layers.get(&index)
                                    && let Some((start, end)) = selection.bounds_on_page(index, glyphs.len())
                                {
                                    paint_text_selection(ui.painter(), glyphs, start, end, page_rect, page_sizes[index]);
                                    if index == selection.focus.page {
                                        selection_action_rect = text_selection_screen_rect(
                                            glyphs, start, end, page_rect, page_sizes[index],
                                        );
                                    }
                                }
                                let pointer = ctx.pointer_latest_pos();
                                let pointer_on_annotation = pointer.is_some_and(|pointer| {
                                    annotation_at(
                                        &page_annotations,
                                        pointer,
                                        page_rect,
                                        page_sizes[index],
                                    )
                                    .is_some()
                                });
                                let annotation_ui = annotation_context_menu(
                                    ui,
                                    &page_annotations,
                                    page_rect,
                                    page_sizes[index],
                                    self.tool_mode == ToolMode::Eraser,
                                );
                                if let Some(id) = annotation_ui.clicked_comment {
                                    self.pinned_comment = Some(id);
                                }
                                let annotation_consumed = annotation_ui.action.is_some()
                                    || annotation_ui.clicked_comment.is_some();
                                if let Some(action) = annotation_ui.action {
                                    match action {
                                        AnnotationContextAction::AddComment {
                                            page,
                                            position,
                                            parent,
                                        } => {
                                            self.note_draft = Some(NoteDraft {
                                                annotation_id: None,
                                                parent,
                                                page,
                                                position,
                                                screen_anchor: Some(pdf_point_to_screen(
                                                    position,
                                                    page_rect,
                                                    page_sizes[index],
                                                )),
                                                text: String::new(),
                                            });
                                        }
                                        AnnotationContextAction::EditComment(id) => {
                                            if let Some(annotation) = page_annotations
                                                .iter()
                                                .find(|annotation| annotation.id == id)
                                                && let AnnotationKind::Note {
                                                    position,
                                                    text,
                                                    parent,
                                                    ..
                                                } = &annotation.kind
                                            {
                                                self.note_draft = Some(NoteDraft {
                                                    annotation_id: Some(id),
                                                    parent: *parent,
                                                    page: annotation.page,
                                                    position: *position,
                                                    screen_anchor: Some(pdf_point_to_screen(
                                                        *position,
                                                        page_rect,
                                                        page_sizes[index],
                                                    )),
                                                    text: text.clone(),
                                                });
                                            }
                                        }
                                        AnnotationContextAction::Delete(id) => {
                                            if self.remove_annotation(id, true) {
                                                self.status = "Annotation deleted.".to_owned();
                                            }
                                        }
                                    }
                                }
                                let visible_comment = annotation_ui.hovered_comment.or_else(|| {
                                    self.pinned_comment.filter(|id| {
                                        page_annotations.iter().any(|annotation| annotation.id == *id)
                                    })
                                });
                                if let Some(id) = visible_comment
                                    && let Some(annotation) = page_annotations.iter().find(|annotation| annotation.id == id)
                                    && show_comment_card(&ctx, annotation, page_rect, page_sizes[index], self.pinned_comment == Some(id))
                                {
                                    self.pinned_comment = None;
                                }

                                let mut pointer_on_link = false;
                                let mut activated_link = None;
                                let primary_clicked =
                                    ctx.input(|input| input.pointer.primary_clicked());
                                if let Some(links) = self.link_layers.get(&index).cloned() {
                                    for link in &links {
                                        let link_rect = pdf_bounds_screen_rect(
                                            link.bounds,
                                            page_rect,
                                            page_sizes[index],
                                        );
                                        if pointer.is_some_and(|pointer| link_rect.contains(pointer)) {
                                            pointer_on_link = true;
                                            ctx.set_cursor_icon(CursorIcon::PointingHand);
                                            ui.painter().rect_stroke(
                                                link_rect.expand(1.5),
                                                2.0,
                                                Stroke::new(1.25, Color32::from_rgb(68, 137, 255)),
                                                StrokeKind::Inside,
                                            );
                                            if primary_clicked {
                                                activated_link = Some(link.target.clone());
                                            }
                                        }
                                    }
                                }
                                if let Some(target) = activated_link {
                                    self.activate_link(target, &ctx);
                                }

                                if !pointer_on_link
                                    && !annotation_consumed
                                    && (!pointer_on_annotation
                                        || self.tool_mode == ToolMode::Eraser)
                                {
                                    let source_size = page_sizes[index];
                                    let response = ui.interact(
                                        page_rect,
                                        ui.id().with(("pdf-tool-layer", index)),
                                        Sense::click_and_drag(),
                                    );
                                    match self.tool_mode {
                                        ToolMode::Select | ToolMode::Highlight => {
                                            if let Some(glyphs) = self.text_layers.get(&index)
                                                && let Some(pointer) = response.interact_pointer_pos()
                                            {
                                                if response.drag_started() || response.clicked() {
                                                    if let Some(glyph) = nearest_glyph(glyphs, page_rect, source_size, pointer) {
                                                        let position = TextPosition { page: index, glyph };
                                                        self.text_selection = Some(TextSelection { anchor: position, focus: position });
                                                        self.text_selection_dragging = response.drag_started();
                                                        self.pending_selection_highlight = false;
                                                    }
                                                } else if response.dragged()
                                                    && self.text_selection_dragging
                                                    && page_rect.contains(pointer)
                                                    && let Some(glyph) = nearest_drag_glyph(glyphs, page_rect, source_size, pointer)
                                                    && let Some(selection) = self.text_selection.as_mut()
                                                {
                                                    selection.focus = TextPosition { page: index, glyph };
                                                }
                                                if response.hovered() { ctx.set_cursor_icon(CursorIcon::Text); }
                                            }
                                        }
                                        ToolMode::Pen | ToolMode::Signature => {
                                            if response.hovered() { ctx.set_cursor_icon(CursorIcon::Crosshair); }
                                            if response.drag_started() { self.ink_points.clear(); }
                                            if (response.dragged() || response.drag_started())
                                                && let Some(pointer) = response.interact_pointer_pos()
                                            {
                                                self.ink_points.push(screen_point_to_pdf(pointer, page_rect, source_size));
                                            }
                                            if !self.ink_points.is_empty() {
                                                paint_ink_preview(ui.painter(), &self.ink_points, page_rect, source_size, self.annotation_color);
                                            }
                                            if response.drag_stopped() && self.ink_points.len() > 1 {
                                                let points = std::mem::take(&mut self.ink_points);
                                                let color = if self.tool_mode == ToolMode::Signature {
                                                    [28, 28, 30, 255]
                                                } else {
                                                    [self.annotation_color.r(), self.annotation_color.g(), self.annotation_color.b(), 230]
                                                };
                                                self.add_annotation(index, AnnotationKind::Ink { points, color, width: if self.tool_mode == ToolMode::Signature { 1.8 } else { 2.2 } });
                                                self.status = if self.tool_mode == ToolMode::Signature {
                                                    "Signature saved locally.".to_owned()
                                                } else {
                                                    "Drawing saved locally.".to_owned()
                                                };
                                            }
                                        }
                                        ToolMode::Eraser => {
                                            if response.hovered() {
                                                ctx.set_cursor_icon(CursorIcon::None);
                                                if let Some(pointer) = response.hover_pos() {
                                                    paint_eraser_cursor(ui.painter(), pointer);
                                                }
                                            }
                                            if response.drag_started() {
                                                self.push_annotation_history();
                                                self.eraser_gesture_active = true;
                                            }
                                            let clicked = response.clicked();
                                            if clicked && !self.eraser_gesture_active {
                                                self.push_annotation_history();
                                            }
                                            if (clicked || response.drag_started() || response.dragged())
                                                && let Some(pointer) = response.interact_pointer_pos()
                                                && let Some(id) = annotation_at(&page_annotations, pointer, page_rect, source_size)
                                                && self.remove_annotation(id, false)
                                            {
                                                self.status = "Annotation erased. Undo with Ctrl+Z.".to_owned();
                                            }
                                            if response.drag_stopped() {
                                                self.eraser_gesture_active = false;
                                            }
                                        }
                                        ToolMode::Note => {
                                            if response.hovered() { ctx.set_cursor_icon(CursorIcon::Crosshair); }
                                            if response.clicked() && let Some(pointer) = response.interact_pointer_pos() {
                                                self.note_draft = Some(NoteDraft {
                                                    annotation_id: None,
                                                    parent: None,
                                                    page: index,
                                                    position: screen_point_to_pdf(pointer, page_rect, source_size),
                                                    screen_anchor: Some(pointer),
                                                    text: String::new(),
                                                });
                                            }
                                        }
                                        ToolMode::TextBox | ToolMode::Stamp => {
                                            if response.hovered() { ctx.set_cursor_icon(CursorIcon::Crosshair); }
                                            if response.clicked() && let Some(pointer) = response.interact_pointer_pos() {
                                                let position = screen_point_to_pdf(pointer, page_rect, source_size);
                                                self.text_draft = Some(TextDraft {
                                                    page: index,
                                                    rect: [position[0], position[1] - 38.0, position[0] + 170.0, position[1]],
                                                    text: if self.tool_mode == ToolMode::Stamp { "APPROVED".to_owned() } else { String::new() },
                                                    stamp: self.tool_mode == ToolMode::Stamp,
                                                });
                                            }
                                        }
                                        ToolMode::Rectangle | ToolMode::Ellipse => {
                                            if response.hovered() { ctx.set_cursor_icon(CursorIcon::Crosshair); }
                                            if response.drag_started() { self.drag_start = response.interact_pointer_pos(); }
                                            if let (Some(start), Some(end)) = (self.drag_start, response.interact_pointer_pos()) {
                                                let preview = Rect::from_two_pos(start, end).intersect(page_rect);
                                                if self.tool_mode == ToolMode::Ellipse {
                                                    ui.painter().circle_stroke(preview.center(), preview.width().min(preview.height()) * 0.5, Stroke::new(1.6, self.annotation_color));
                                                } else {
                                                    ui.painter().rect_stroke(preview, 0.0, Stroke::new(1.6, self.annotation_color), StrokeKind::Inside);
                                                }
                                                if response.drag_stopped() && preview.width() > 5.0 && preview.height() > 5.0 {
                                                    self.drag_start = None;
                                                    self.add_annotation(index, AnnotationKind::Shape {
                                                        rect: screen_rect_to_pdf(preview, page_rect, source_size),
                                                        color: [self.annotation_color.r(), self.annotation_color.g(), self.annotation_color.b(), 255],
                                                        width: 1.8,
                                                        ellipse: self.tool_mode == ToolMode::Ellipse,
                                                    });
                                                    self.status = "Shape saved locally.".to_owned();
                                                }
                                            }
                                        }
                                        ToolMode::Attachment => {
                                            if response.hovered() { ctx.set_cursor_icon(CursorIcon::Crosshair); }
                                            if response.clicked()
                                                && let Some(pointer) = response.interact_pointer_pos()
                                                && let Some(path) = rfd::FileDialog::new().pick_file()
                                            {
                                                self.add_annotation(index, AnnotationKind::Attachment {
                                                    position: screen_point_to_pdf(pointer, page_rect, source_size),
                                                    path,
                                                });
                                                self.status = "File reference attached locally.".to_owned();
                                            }
                                        }
                                        ToolMode::Ocr | ToolMode::MathOcr => {
                                            if response.hovered() { ctx.set_cursor_icon(CursorIcon::Crosshair); }
                                            if response.drag_started() { self.drag_start = response.interact_pointer_pos(); }
                                            if let (Some(start), Some(end)) = (self.drag_start, response.interact_pointer_pos()) {
                                                let snip = Rect::from_two_pos(start, end).intersect(page_rect);
                                                ui.painter().rect_filled(snip, 2.0, Color32::from_rgba_unmultiplied(82, 170, 255, 38));
                                                ui.painter().rect_stroke(snip, 2.0, Stroke::new(1.5, Color32::from_rgb(82, 170, 255)), StrokeKind::Inside);
                                                if response.drag_stopped() && snip.width() > 8.0 && snip.height() > 8.0 {
                                                    self.drag_start = None;
                                                    let bounds = screen_rect_to_pdf(snip, page_rect, source_size);
                                                    let embedded = self.text_layers.get(&index).map(|glyphs| text_in_pdf_region(glyphs, bounds)).unwrap_or_default();
                                                    if self.tool_mode != ToolMode::MathOcr && !embedded.trim().is_empty() {
                                                        self.ocr_popup = Some(OcrPopup { text: embedded, detail: "Exact PDF text layer - instant - no OCR needed".to_owned(), busy: false });
                                                    } else {
                                                        let request_id = self.next_ocr_request;
                                                        self.next_ocr_request = self.next_ocr_request.wrapping_add(1);
                                                        let math = self.tool_mode == ToolMode::MathOcr;
                                                        if math { self.math_ocr_requests.insert(request_id); }
                                                        self.ocr_popup = Some(OcrPopup {
                                                            text: String::new(),
                                                            detail: if math {
                                                                "Preparing formula crop; first use may finish the one-time model download...".to_owned()
                                                            } else {
                                                                "Preparing high-resolution crop...".to_owned()
                                                            },
                                                            busy: true,
                                                        });
                                                        let _ = self.worker.commands.send(RenderCommand::RenderRegion { page_index: index, bounds, generation, request_id });
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if self.tool_mode == ToolMode::Select && !self.text_selection_dragging
                            && let Some(selection) = self.text_selection
                            && selection.anchor != selection.focus
                            && let Some(rect) = selection_action_rect
                            && let Some(action) = selection_action_bar(&ctx, rect)
                        {
                            match action {
                                SelectionAction::Copy => {
                                    if let Some(text) = self.selected_text() {
                                        ctx.copy_text(text);
                                        self.status = "Selected text copied.".to_owned();
                                    }
                                }
                                SelectionAction::Highlight => self.highlight_text_selection(),
                            }
                        }
                    });
                // This includes normal wheel scrolling, scroll-bar drags, and
                // offset clamping. Use the actual state, never a predicted value.
                self.document_scroll_offset = scroll_output.state.offset;
                self.current_page = visible_page;
                if let Some(path) = self.current_path.clone() {
                    self.state.reading_positions.insert(path, visible_page);
                }
            });
    }

    fn mode_controls(&mut self, ctx: &egui::Context) {
        if !self.presentation_mode && !self.reader_mode && self.toolbar_visible {
            return;
        }
        let bounds = ctx.content_rect();
        let control_x = if self.presentation_mode || self.reader_mode {
            bounds.left() + 8.0
        } else {
            bounds.right() - 34.0
        };
        egui::Area::new(egui::Id::new("reading-mode-controls"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos2(control_x, bounds.top() + 8.0))
            .show(ctx, |ui| {
                Frame::new()
                    .fill(Color32::from_rgba_unmultiplied(29, 29, 29, 232))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(70, 70, 70)))
                    .corner_radius(CornerRadius::same(7))
                    .inner_margin(Margin::same(2))
                    .show(ui, |ui| {
                        if self.presentation_mode {
                            if mini_mode_icon_button(
                                ui,
                                ModeControlIcon::Exit,
                                "Exit presentation (Esc)",
                            )
                            .clicked()
                            {
                                self.set_presentation_mode(ctx, false);
                            }
                        } else if self.reader_mode {
                            if mini_mode_icon_button(
                                ui,
                                ModeControlIcon::Exit,
                                "Exit reader mode (Esc)",
                            )
                            .clicked()
                            {
                                self.set_reader_mode(ctx, false);
                            }
                        } else if mini_mode_icon_button(
                            ui,
                            ModeControlIcon::ShowToolbar,
                            "Show toolbar",
                        )
                        .clicked()
                        {
                            self.toolbar_visible = true;
                        }
                    });
            });
    }

    fn folder_browser(&mut self, ui: &mut egui::Ui) {
        let Some(directory) = self.browser_directory.clone() else {
            return;
        };
        let entries = self.browser_entries.clone();
        let scanning = self.browser_scan.is_some();
        let pdf_total = entries.iter().filter(|entry| !entry.is_directory).count();
        let mut browse = None;
        let mut open = None;
        let mut close = false;
        let mut open_system_folder = false;
        Frame::new()
            .fill(theme::CANVAS_BG)
            .inner_margin(Margin::symmetric(18, 16))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Files").size(19.0).strong());
                        ui.label(
                            RichText::new(format!(
                                "{}  ·  {} PDFs in this folder tree",
                                directory.display(),
                                pdf_total
                            ))
                            .size(12.0)
                            .color(theme::MUTED),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Back to document").clicked() {
                            close = true;
                        }
                        if ui.button("Open in Files").clicked() {
                            open_system_folder = true;
                        }
                        if let Some(parent) = directory.parent()
                            && ui.button("Up").clicked()
                        {
                            browse = Some(parent.to_path_buf());
                        }
                    });
                });
                ui.add_space(12.0);
                Frame::new()
                    .fill(Color32::from_rgb(26, 26, 26))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(52, 52, 52)))
                    .corner_radius(CornerRadius::same(5))
                    .inner_margin(Margin::ZERO)
                    .show(ui, |ui| {
                        let (header, _) = ui
                            .allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::hover());
                        ui.painter().rect_filled(
                            header,
                            CornerRadius::same(5),
                            Color32::from_rgb(34, 34, 34),
                        );
                        let location_x = header.left() + header.width() * 0.62;
                        ui.painter().text(
                            pos2(header.left() + 34.0, header.center().y),
                            Align2::LEFT_CENTER,
                            "Name",
                            FontId::proportional(11.0),
                            theme::MUTED,
                        );
                        ui.painter().text(
                            pos2(location_x, header.center().y),
                            Align2::LEFT_CENTER,
                            "Location / count",
                            FontId::proportional(11.0),
                            theme::MUTED,
                        );
                        ScrollArea::vertical()
                            .id_salt("folder-browser")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if scanning {
                                    ui.horizontal(|ui| {
                                        ui.spinner();
                                        ui.label(
                                            RichText::new("Scanning all nested folders...")
                                                .color(theme::MUTED),
                                        );
                                    });
                                    ui.add_space(8.0);
                                } else if entries.is_empty() {
                                    ui.label(
                                        RichText::new("No PDF files were found here.")
                                            .color(theme::MUTED),
                                    );
                                }
                                for entry in entries {
                                    let (rect, response) = ui.allocate_exact_size(
                                        vec2(ui.available_width(), 36.0),
                                        Sense::click(),
                                    );
                                    ui.painter().line_segment(
                                        [rect.left_bottom(), rect.right_bottom()],
                                        Stroke::new(1.0, Color32::from_rgb(42, 42, 42)),
                                    );
                                    if response.hovered() {
                                        ui.painter().rect_filled(
                                            rect,
                                            0.0,
                                            Color32::from_rgb(43, 43, 43),
                                        );
                                    }
                                    let center = pos2(rect.left() + 15.0, rect.center().y);
                                    if entry.is_directory {
                                        paint_folder_icon(ui.painter(), center, theme::MUTED, 1.0);
                                    } else {
                                        paint_file_icon(ui.painter(), center, theme::MUTED, 1.0);
                                    }
                                    ui.painter().text(
                                        pos2(rect.left() + 34.0, rect.center().y),
                                        Align2::LEFT_CENTER,
                                        file_name(&entry.path),
                                        FontId::proportional(13.0),
                                        Color32::from_rgb(214, 214, 214),
                                    );
                                    let detail = if entry.is_directory {
                                        format!("{} PDFs", entry.pdf_count)
                                    } else {
                                        entry
                                            .relative
                                            .parent()
                                            .map(|path| path.display().to_string())
                                            .filter(|path| !path.is_empty())
                                            .unwrap_or_else(|| ".".to_owned())
                                    };
                                    ui.painter().text(
                                        pos2(rect.left() + rect.width() * 0.62, rect.center().y),
                                        Align2::LEFT_CENTER,
                                        elide(&detail, 44),
                                        FontId::proportional(11.5),
                                        theme::MUTED,
                                    );
                                    if response.clicked() {
                                        if entry.is_directory {
                                            browse = Some(entry.path);
                                        } else {
                                            open = Some(entry.path);
                                        }
                                    }
                                }
                            });
                    });
            });
        if open_system_folder {
            self.open_current_directory();
        }
        if close {
            self.browser_directory = None;
            self.browser_entries.clear();
        } else if let Some(path) = browse {
            self.open_folder_browser(path);
        } else if let Some(path) = open {
            self.open_document(path);
        }
    }

    fn selected_text(&self) -> Option<String> {
        let selection = self.text_selection?;
        let (first, last) = selection.ordered();
        let mut parts = Vec::new();
        for page in first.page..=last.page {
            let glyphs = self.text_layers.get(&page)?;
            if let Some((start, end)) = selection.bounds_on_page(page, glyphs.len())
                && let Some(text) = selected_glyph_text(glyphs, start, end)
            {
                parts.push(text);
            }
        }
        (!parts.is_empty()).then(|| parts.join("\n"))
    }

    fn selection_text_ready(&self) -> bool {
        let Some(selection) = self.text_selection else {
            return false;
        };
        let (first, last) = selection.ordered();
        (first.page..=last.page).all(|page| self.text_layers.contains_key(&page))
    }

    fn highlight_text_selection(&mut self) {
        let Some(selection) = self.text_selection else {
            return;
        };
        if !self.selection_text_ready() {
            self.pending_selection_highlight = true;
            let (first, last) = selection.ordered();
            for page in first.page..=last.page {
                self.request_text(page, self.generation);
            }
            self.status = "Finishing text extraction before highlighting...".to_owned();
            return;
        }
        self.pending_selection_highlight = false;
        self.text_selection = None;
        let (first, last) = selection.ordered();
        let mut saved = 0;
        for page in first.page..=last.page {
            let Some(glyphs) = self.text_layers.get(&page) else {
                continue;
            };
            let Some((start, end)) = selection.bounds_on_page(page, glyphs.len()) else {
                continue;
            };
            let rects = glyphs[start..=end]
                .iter()
                .filter_map(|glyph| glyph.bounds)
                .collect::<Vec<_>>();
            if !rects.is_empty() {
                self.add_annotation(
                    page,
                    AnnotationKind::Highlight {
                        rects,
                        color: [
                            self.annotation_color.r(),
                            self.annotation_color.g(),
                            self.annotation_color.b(),
                            86,
                        ],
                    },
                );
                saved += 1;
            }
        }
        if saved > 0 {
            self.status = "Highlight saved locally.".to_owned();
        }
    }

    fn rename_dialog(&mut self, ctx: &egui::Context) {
        let Some(target) = self.rename_target.clone() else {
            return;
        };
        let title = match target {
            RenameTarget::Project(_) => "Rename project",
            RenameTarget::File(_) => "Rename PDF display name",
        };
        let mut save = false;
        let mut open = true;
        egui::Window::new(title)
            .open(&mut open)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, -80.0))
            .collapsible(false)
            .resizable(false)
            .default_width(292.0)
            .min_width(292.0)
            .max_width(292.0)
            .show(ctx, |ui| {
                let response = ui.add_sized(
                    [ui.available_width(), 28.0],
                    egui::TextEdit::singleline(&mut self.rename_buffer).hint_text("Display name"),
                );
                if self.rename_focus_requested {
                    response.request_focus();
                    self.rename_focus_requested = false;
                }
                let enter =
                    response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
                ui.add_space(5.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            !self.rename_buffer.trim().is_empty(),
                            egui::Button::new("Save"),
                        )
                        .clicked()
                        || enter
                    {
                        save = !self.rename_buffer.trim().is_empty();
                    }
                });
            });

        if !open {
            self.rename_target = None;
        } else if save {
            match target {
                RenameTarget::Project(index) => {
                    if let Some(project) = self.state.projects.get_mut(index) {
                        project.name = self.rename_buffer.trim().to_owned();
                    }
                }
                RenameTarget::File(path) => {
                    self.state.set_file_alias(&path, self.rename_buffer.clone());
                }
            }
            self.rename_target = None;
            self.persist();
        }
    }

    fn note_dialog(&mut self, ctx: &egui::Context) {
        let Some(draft) = self.note_draft.as_mut() else {
            return;
        };
        let editing = draft.annotation_id.is_some();
        let mut save = false;
        let mut open = true;
        let mut window = egui::Window::new(if editing {
            "Edit comment"
        } else {
            "Add comment"
        })
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(300.0);
        if let Some(anchor) = draft.screen_anchor {
            window = window.current_pos(anchor + vec2(18.0, -24.0));
        } else {
            window = window.anchor(Align2::CENTER_CENTER, vec2(0.0, -40.0));
        }
        window.show(ctx, |ui| {
            let editor = ui.add_sized(
                [ui.available_width(), 76.0],
                egui::TextEdit::multiline(&mut draft.text)
                    .hint_text("Write a comment...")
                    .desired_rows(3),
            );
            let enter_to_save = editor.has_focus()
                && ui.input(|input| input.key_pressed(Key::Enter) && !input.modifiers.shift);
            if enter_to_save && !draft.text.trim().is_empty() {
                save = true;
            }
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Shift+Enter for a new line")
                        .size(10.5)
                        .color(theme::MUTED),
                );
                if ui
                    .add_enabled(
                        !draft.text.trim().is_empty(),
                        egui::Button::new(if editing {
                            "Save changes"
                        } else {
                            "Add comment"
                        }),
                    )
                    .clicked()
                {
                    save = true;
                }
            });
        });
        if !open {
            self.note_draft = None;
        }
        if save && let Some(draft) = self.note_draft.take() {
            let text = draft.text.trim().to_owned();
            if let Some(id) = draft.annotation_id {
                if self.update_comment(id, text) {
                    self.status = "Comment updated.".to_owned();
                }
            } else {
                self.add_annotation(
                    draft.page,
                    AnnotationKind::Note {
                        position: draft.position,
                        text,
                        color: [
                            self.annotation_color.r(),
                            self.annotation_color.g(),
                            self.annotation_color.b(),
                            255,
                        ],
                        parent: draft.parent,
                    },
                );
                self.status = "Comment saved locally.".to_owned();
            }
        }
    }

    fn view_settings_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_view_settings {
            return;
        }
        let mut open = true;
        let mut toolbar_ratio = self.state.chrome_scale_ratio;
        let mut sidebar_ratio = self.state.sidebar_scale_ratio;
        let mut file_icons = self.state.sidebar_file_icons;
        let mut changed = false;
        egui::Window::new("View settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(300.0)
            .show(ctx, |ui| {
                ui.label(RichText::new("Interface scale").strong());
                ui.label(
                    RichText::new("Keep controls compact while making the PDF the visual focus.")
                        .size(11.5)
                        .color(theme::MUTED),
                );
                ui.add_space(8.0);
                ui.label(RichText::new("Top bar").size(12.0));
                changed |= ui
                    .add(
                        egui::Slider::new(&mut toolbar_ratio, 0.4..=1.0)
                            .step_by(0.05)
                            .custom_formatter(|value, _| {
                                format!("{}%", (value * 100.0).round() as i32)
                            }),
                    )
                    .changed();
                ui.add_space(7.0);
                ui.label(RichText::new("File explorer").size(12.0));
                changed |= ui
                    .add(
                        egui::Slider::new(&mut sidebar_ratio, 0.5..=1.25)
                            .step_by(0.05)
                            .custom_formatter(|value, _| {
                                format!("{}%", (value * 100.0).round() as i32)
                            }),
                    )
                    .changed();
                ui.add_space(6.0);
                if ui.small_button("Reset defaults (60% / 80%)").clicked() {
                    toolbar_ratio = 0.6;
                    sidebar_ratio = 0.8;
                    changed = true;
                }
                ui.add_space(10.0);
                ui.label(RichText::new("PDF icons in explorer").size(12.0));
                changed |= ui
                    .radio_value(&mut file_icons, SidebarFileIcons::Standard, "Standard")
                    .changed();
                changed |= ui
                    .radio_value(
                        &mut file_icons,
                        SidebarFileIcons::VsCode,
                        "VS Code-style PDF",
                    )
                    .changed();
                changed |= ui
                    .radio_value(&mut file_icons, SidebarFileIcons::None, "No icons")
                    .changed();
            });
        if !open {
            self.show_view_settings = false;
        }
        if changed {
            self.state.chrome_scale_ratio = toolbar_ratio;
            self.state.sidebar_scale_ratio = sidebar_ratio;
            self.state.sidebar_file_icons = file_icons;
            self.state.sidebar_file_icons_initialized = true;
            ctx.set_zoom_factor(chrome_zoom(self.state.ui_scale, toolbar_ratio));
            self.persist();
        }
    }

    fn save_to_source_dialog(&mut self, ctx: &egui::Context) {
        if !self.confirm_save_to_source {
            return;
        }
        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        egui::Window::new("Save annotations to PDF?")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(360.0)
            .show(ctx, |ui| {
                ui.label("This writes SnapPDF annotations into the PDF on disk so other viewers can see them.");
                ui.add_space(6.0);
                ui.label(
                    RichText::new("A timestamped backup of the original PDF will be created first.")
                        .size(11.5)
                        .color(theme::MUTED),
                );
                ui.add_space(10.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Save to PDF").clicked() {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if !open || cancel {
            self.confirm_save_to_source = false;
        }
        if save {
            self.confirm_save_to_source = false;
            self.save_annotations_to_source();
        }
    }

    fn ocr_result_dialog(&mut self, ctx: &egui::Context) {
        let Some(popup) = self.ocr_popup.as_mut() else {
            return;
        };
        let mut open = true;
        egui::Window::new("Snip OCR")
            .open(&mut open)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .collapsible(false)
            .resizable(false)
            .fixed_size(vec2(560.0, 350.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if popup.busy {
                        ui.spinner();
                    }
                    ui.label(RichText::new(&popup.detail).size(12.0).color(theme::MUTED));
                });
                ui.add_space(6.0);
                // A fixed scroll region keeps the action row in view even when
                // OCR returns several pages of text.
                ui.allocate_ui_with_layout(
                    vec2(ui.available_width(), 230.0),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ScrollArea::vertical()
                            .id_salt("ocr-result-text-scroll")
                            .show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut popup.text)
                                        .desired_width(ui.available_width())
                                        .desired_rows(12)
                                        .hint_text("Recognized text will appear here..."),
                                );
                            });
                    },
                );
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!popup.text.is_empty(), egui::Button::new("Copy text"))
                        .clicked()
                    {
                        ctx.copy_text(popup.text.clone());
                        popup.detail = "Copied to clipboard.".to_owned();
                    }
                    ui.label(
                        RichText::new("Nothing is copied automatically.")
                            .size(11.0)
                            .color(theme::MUTED),
                    );
                });
            });
        if !open {
            self.ocr_popup = None;
        }
    }

    fn text_annotation_dialog(&mut self, ctx: &egui::Context) {
        let Some(draft) = self.text_draft.as_mut() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        egui::Window::new(if draft.stamp {
            "Add stamp"
        } else {
            "Add text box"
        })
        .open(&mut open)
        .anchor(Align2::CENTER_CENTER, vec2(0.0, -40.0))
        .collapsible(false)
        .resizable(true)
        .default_width(380.0)
        .show(ctx, |ui| {
            ui.add_sized(
                [ui.available_width(), if draft.stamp { 34.0 } else { 120.0 }],
                if draft.stamp {
                    egui::TextEdit::singleline(&mut draft.text).hint_text("Stamp text")
                } else {
                    egui::TextEdit::multiline(&mut draft.text).hint_text("Text")
                },
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(!draft.text.trim().is_empty(), egui::Button::new("Add"))
                    .clicked()
                {
                    save = true;
                }
            });
        });
        if !open {
            self.text_draft = None;
        } else if save && let Some(draft) = self.text_draft.take() {
            let color = [
                self.annotation_color.r(),
                self.annotation_color.g(),
                self.annotation_color.b(),
                255,
            ];
            if draft.stamp {
                self.add_annotation(
                    draft.page,
                    AnnotationKind::Stamp {
                        position: [draft.rect[0], draft.rect[3]],
                        text: draft.text.trim().to_owned(),
                        color,
                    },
                );
            } else {
                self.add_annotation(
                    draft.page,
                    AnnotationKind::TextBox {
                        rect: draft.rect,
                        text: draft.text.trim().to_owned(),
                        color,
                    },
                );
            }
            self.status = "Annotation saved locally.".to_owned();
        }
    }

    fn open_with_dialog(&mut self, ctx: &egui::Context) {
        let Some(path) = self.open_with_path.clone() else {
            return;
        };
        let apps = self.external_apps.clone();
        let mut selected = None;
        let mut open = true;
        egui::Window::new("Open with")
            .open(&mut open)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, -30.0))
            .collapsible(false)
            .resizable(false)
            .default_width(390.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(file_name(&path))
                        .size(13.5)
                        .strong()
                        .color(Color32::from_rgb(224, 226, 231)),
                );
                ui.label(
                    RichText::new("Choose another application registered for PDF files.")
                        .size(11.5)
                        .color(theme::MUTED),
                );
                ui.add_space(8.0);
                for app in &apps {
                    if ui
                        .add_sized(
                            [ui.available_width(), 34.0],
                            egui::Button::new(RichText::new(&app.name).size(13.5)).frame(false),
                        )
                        .clicked()
                    {
                        selected = Some(app.clone());
                    }
                }
                if let Some(error) = &self.open_with_error {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(error)
                            .size(11.5)
                            .color(Color32::from_rgb(233, 126, 132)),
                    );
                }
            });
        if !open {
            self.open_with_path = None;
        } else if let Some(app) = selected {
            self.launch_external_app(&app, &path);
        }
    }

    fn print_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.print_dialog.as_mut() else {
            return;
        };
        let mut open = true;
        let mut submit = false;
        egui::Window::new("Print PDF")
            .open(&mut open)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, -30.0))
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .show(ctx, |ui| {
                if dialog.printers.is_empty() {
                    ui.label(
                        RichText::new(
                            "No printer was reported by CUPS. You can still submit to the system default printer.",
                        )
                        .color(theme::MUTED),
                    );
                } else {
                    ui.label(RichText::new("Printer").size(11.0).color(theme::MUTED));
                    egui::ComboBox::from_id_salt("printer-picker")
                        .selected_text(if dialog.printer.is_empty() {
                            "System default"
                        } else {
                            &dialog.printer
                        })
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            for printer in &dialog.printers {
                                ui.selectable_value(
                                    &mut dialog.printer,
                                    printer.clone(),
                                    printer,
                                );
                            }
                        });
                }
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Pages (optional, for example 1-4,7)")
                        .size(11.0)
                        .color(theme::MUTED),
                );
                ui.add_sized(
                    [ui.available_width(), 30.0],
                    egui::TextEdit::singleline(&mut dialog.page_range),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label("Copies");
                    ui.add(egui::DragValue::new(&mut dialog.copies).range(1..=99));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Print").clicked() {
                            submit = true;
                        }
                    });
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Printing starts only after you click Print.")
                        .size(11.0)
                        .color(theme::MUTED),
                );
            });
        if !open {
            self.print_dialog = None;
        } else if submit {
            self.submit_print();
        }
    }

    fn empty_state(&mut self, ui: &mut egui::Ui) {
        ui.centered_and_justified(|ui| {
            ui.vertical_centered(|ui| {
                ui.set_max_width(238.0);
                let action_frame = |ui: &mut egui::Ui, label: &str, shortcut: &str| {
                    let response = Frame::new()
                        .stroke(Stroke::new(1.0, Color32::from_rgb(58, 58, 58)))
                        .corner_radius(CornerRadius::same(18))
                        .inner_margin(Margin::symmetric(14, 8))
                        .show(ui, |ui| {
                            ui.set_width(208.0);
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(label).size(14.0));
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(
                                        RichText::new(shortcut).size(11.5).color(theme::MUTED),
                                    );
                                });
                            });
                        });
                    response.response.interact(Sense::click())
                };

                if action_frame(ui, "Open PDF", "Ctrl+O").clicked() {
                    self.open_file_dialog();
                }
                ui.add_space(7.0);
                if action_frame(ui, "Add project", "Ctrl+K  Ctrl+O").clicked() {
                    self.add_project_dialog();
                }
                if !self.renderer_ready {
                    ui.add_space(12.0);
                    ui.spinner();
                }
            });
        });
    }
}

impl eframe::App for SnapPdfApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame_number = self.frame_number.wrapping_add(1);
        self.handle_events(ctx);
        self.keyboard_shortcuts(ctx);
        self.dropped_files(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.navigation_rail(ui);
        self.sidebar(ui);
        self.toolbar(ui);
        self.viewer(ui);
        self.mode_controls(ui.ctx());
        self.rename_dialog(ui.ctx());
        self.note_dialog(ui.ctx());
        self.view_settings_dialog(ui.ctx());
        self.save_to_source_dialog(ui.ctx());
        self.text_annotation_dialog(ui.ctx());
        self.ocr_result_dialog(ui.ctx());
        self.open_with_dialog(ui.ctx());
        self.print_dialog(ui.ctx());
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = self.worker.commands.send(RenderCommand::Shutdown);
        let _ = self.ocr_worker.commands.send(OcrCommand::Shutdown);
        self.persist();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NavIcon {
    Recent,
    File,
    Folder,
}

#[derive(Clone, Copy)]
enum RailIcon {
    Files,
    Contents,
    Search,
    Settings,
}

fn rail_button(ui: &mut egui::Ui, selected: bool, icon: RailIcon) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(38.0, 38.0), Sense::click());
    if selected || response.hovered() {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(5),
            if selected {
                Color32::from_rgb(55, 55, 55)
            } else {
                Color32::from_rgb(39, 39, 39)
            },
        );
    }
    let color = if selected {
        Color32::from_rgb(224, 224, 224)
    } else {
        Color32::from_rgb(148, 148, 148)
    };
    let center = rect.center();
    match icon {
        RailIcon::Files => paint_folder_icon(ui.painter(), center, color, 1.0),
        RailIcon::Contents => {
            for y in [-6.0, 0.0, 6.0] {
                ui.painter()
                    .circle_filled(pos2(center.x - 7.0, center.y + y), 1.3, color);
                ui.painter().line_segment(
                    [
                        pos2(center.x - 3.0, center.y + y),
                        pos2(center.x + 8.0, center.y + y),
                    ],
                    Stroke::new(1.4, color),
                );
            }
        }
        RailIcon::Search => {
            ui.painter().circle_stroke(
                pos2(center.x - 2.0, center.y - 2.0),
                6.0,
                Stroke::new(1.6, color),
            );
            ui.painter().line_segment(
                [
                    pos2(center.x + 2.5, center.y + 2.5),
                    pos2(center.x + 8.0, center.y + 8.0),
                ],
                Stroke::new(1.6, color),
            );
        }
        RailIcon::Settings => {
            paint_toolbar_icon(ui.painter(), center, ToolbarIcon::Settings, color)
        }
    }
    response
}

struct SidebarRowResponse {
    body: Response,
    options: Response,
}

#[derive(Clone, Copy)]
enum SidebarMenuAction {
    OpenWith,
    OpenInFiles,
    Rename,
    ResetName,
    Pin,
    Unpin,
    Remove,
}

fn build_pdf_tree(root: &Path, files: &[PathBuf]) -> PdfTreeNode {
    let mut tree = PdfTreeNode {
        name: root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Project")
            .to_owned(),
        path: root.to_path_buf(),
        ..Default::default()
    };
    for file in files {
        let relative = file.strip_prefix(root).unwrap_or(file);
        let mut node = &mut tree;
        let mut directory_path = root.to_path_buf();
        if let Some(parent) = relative.parent() {
            for component in parent.components() {
                let name = component.as_os_str().to_string_lossy().to_string();
                directory_path.push(&name);
                node = node
                    .directories
                    .entry(name.clone())
                    .or_insert_with(|| PdfTreeNode {
                        name,
                        path: directory_path.clone(),
                        ..Default::default()
                    });
            }
        }
        node.files.push(file.clone());
    }
    tree
}

#[allow(clippy::too_many_arguments)]
fn render_pdf_tree(
    ui: &mut egui::Ui,
    node: &PdfTreeNode,
    depth: usize,
    current_path: Option<&Path>,
    aliases: &HashMap<PathBuf, String>,
    collapsed: &mut HashSet<PathBuf>,
    pinned_files: &[PathBuf],
    pinned_folders: &[PathBuf],
    file_icons: SidebarFileIcons,
) -> Option<TreeAction> {
    let mut action = None;
    let mut directories: Vec<_> = node.directories.values().collect();
    directories.sort_by_key(|child| !pinned_folders.contains(&child.path));
    for child in directories {
        let is_collapsed = collapsed.contains(&child.path);
        let row = directory_row(
            ui,
            &child.name,
            pdf_tree_count(child),
            !is_collapsed,
            24.0 + depth as f32 * 14.0,
        );
        row.body
            .clone()
            .on_hover_text(child.path.display().to_string());
        if row.body.clicked() && !row.options.clicked() {
            if is_collapsed {
                collapsed.remove(&child.path);
            } else {
                collapsed.insert(child.path.clone());
            }
        }
        if let Some(menu_action) = sidebar_options_popup(
            &row.options,
            false,
            None,
            false,
            pinned_folders.contains(&child.path),
        ) {
            action = match menu_action {
                SidebarMenuAction::Pin => Some(TreeAction::PinFolder(child.path.clone())),
                SidebarMenuAction::Unpin => Some(TreeAction::UnpinFolder(child.path.clone())),
                _ => action,
            };
        }
        if !is_collapsed {
            action = render_pdf_tree(
                ui,
                child,
                depth + 1,
                current_path,
                aliases,
                collapsed,
                pinned_files,
                pinned_folders,
                file_icons,
            )
            .or(action);
        }
    }
    for path in &node.files {
        let label = aliases
            .get(path)
            .cloned()
            .unwrap_or_else(|| file_name(path));
        let selected = current_path == Some(path.as_path());
        let row = nav_row(
            ui,
            &label,
            selected,
            NavIcon::File,
            24.0 + depth as f32 * 14.0,
            file_icons,
        );
        if label.chars().count()
            > max_sidebar_label_chars(&row.body, 24.0 + depth as f32 * 14.0, file_icons)
        {
            row.body.clone().on_hover_text(label.clone());
        }
        if row.body.clicked() && !row.options.clicked() {
            action = Some(TreeAction::Open(path.clone()));
        }
        if let Some(menu_action) = sidebar_options_popup(
            &row.options,
            aliases.contains_key(path),
            None,
            true,
            pinned_files.contains(path),
        ) {
            action = match menu_action {
                SidebarMenuAction::OpenWith => Some(TreeAction::OpenWith(path.clone())),
                SidebarMenuAction::OpenInFiles => Some(TreeAction::OpenInFiles(path.clone())),
                SidebarMenuAction::Rename => Some(TreeAction::Rename(path.clone(), label)),
                SidebarMenuAction::ResetName => Some(TreeAction::ResetName(path.clone())),
                SidebarMenuAction::Pin => Some(TreeAction::PinFile(path.clone())),
                SidebarMenuAction::Unpin => Some(TreeAction::UnpinFile(path.clone())),
                _ => action,
            };
        }
    }
    action
}

fn pdf_tree_count(node: &PdfTreeNode) -> usize {
    node.files.len() + node.directories.values().map(pdf_tree_count).sum::<usize>()
}

fn sidebar_visual_scale(ui: &egui::Ui) -> f32 {
    (ui.spacing().interact_size.y / 18.0).clamp(0.1, 4.0)
}

fn directory_row(
    ui: &mut egui::Ui,
    name: &str,
    count: usize,
    expanded: bool,
    indent: f32,
) -> SidebarRowResponse {
    let scale = sidebar_visual_scale(ui);
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), 30.0 * scale), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(
            rect.shrink2(vec2(1.0, 0.0)),
            CornerRadius::same(5),
            theme::PANEL_BG,
        );
    }
    let x = rect.left() + indent * scale;
    let y = rect.center().y;
    let muted = theme::MUTED;
    let chevron = if expanded {
        [
            pos2(x, y - 2.0 * scale),
            pos2(x + 4.0 * scale, y + 2.0 * scale),
            pos2(x + 8.0 * scale, y - 2.0 * scale),
        ]
    } else {
        [
            pos2(x + 2.0 * scale, y - 4.0 * scale),
            pos2(x + 6.0 * scale, y),
            pos2(x + 2.0 * scale, y + 4.0 * scale),
        ]
    };
    ui.painter().add(egui::Shape::line(
        chevron.to_vec(),
        Stroke::new(1.3 * scale, muted),
    ));
    paint_folder_icon(ui.painter(), pos2(x + 19.0 * scale, y), muted, scale);
    ui.painter().text(
        pos2(x + 32.0 * scale, y),
        Align2::LEFT_CENTER,
        elide(
            name,
            ((rect.right() - x - 38.0 * scale) / (7.0 * scale)).max(5.0) as usize,
        ),
        FontId::proportional(13.0 * scale),
        Color32::from_rgb(202, 202, 202),
    );
    ui.painter().text(
        pos2(rect.right() - 7.0 * scale, y),
        Align2::RIGHT_CENTER,
        count.to_string(),
        FontId::proportional(10.5 * scale),
        theme::MUTED,
    );
    let options = sidebar_options_button(ui, rect, &response);
    SidebarRowResponse {
        body: response,
        options,
    }
}

fn project_header(
    ui: &mut egui::Ui,
    name: &str,
    count: usize,
    expanded: bool,
) -> SidebarRowResponse {
    let scale = sidebar_visual_scale(ui);
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), 38.0 * scale), Sense::click());
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(
            rect.shrink2(vec2(1.0, 1.0)),
            CornerRadius::same(6),
            theme::PANEL_BG,
        );
    }

    let icon_color = theme::MUTED;
    let chevron_x = rect.left() + 8.0 * scale;
    let center_y = rect.center().y;
    let chevron = if expanded {
        [
            pos2(chevron_x, center_y - 2.0 * scale),
            pos2(chevron_x + 4.0 * scale, center_y + 2.0 * scale),
            pos2(chevron_x + 8.0 * scale, center_y - 2.0 * scale),
        ]
    } else {
        [
            pos2(chevron_x + 2.0 * scale, center_y - 4.0 * scale),
            pos2(chevron_x + 6.0 * scale, center_y),
            pos2(chevron_x + 2.0 * scale, center_y + 4.0 * scale),
        ]
    };
    painter.add(egui::Shape::line(
        chevron.to_vec(),
        Stroke::new(1.5 * scale, theme::MUTED),
    ));
    paint_folder_icon(
        painter,
        pos2(rect.left() + 27.0 * scale, center_y),
        icon_color,
        scale,
    );

    let count_text = count.to_string();
    painter.text(
        pos2(rect.right() - 34.0 * scale, center_y),
        Align2::RIGHT_CENTER,
        &count_text,
        FontId::proportional(11.0 * scale),
        theme::MUTED,
    );
    let max_chars = ((rect.width() - 112.0 * scale) / (7.2 * scale)).max(4.0) as usize;
    painter.text(
        pos2(rect.left() + 51.0 * scale, center_y),
        Align2::LEFT_CENTER,
        elide(name, max_chars),
        FontId::proportional(14.0 * scale),
        Color32::from_rgb(218, 220, 225),
    );
    let options = sidebar_options_button(ui, rect, &response);
    SidebarRowResponse {
        body: response,
        options,
    }
}

fn nav_row(
    ui: &mut egui::Ui,
    label: &str,
    selected: bool,
    icon: NavIcon,
    indent: f32,
    file_icons: SidebarFileIcons,
) -> SidebarRowResponse {
    let scale = sidebar_visual_scale(ui);
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), 34.0 * scale), Sense::click());
    let painter = ui.painter();
    if selected {
        painter.rect_filled(
            rect.shrink2(vec2(1.0, 1.0)),
            CornerRadius::same(8),
            theme::ACCENT.gamma_multiply(0.42),
        );
    } else if response.hovered() {
        painter.rect_filled(
            rect.shrink2(vec2(1.0, 1.0)),
            CornerRadius::same(8),
            theme::PANEL_BG,
        );
    }

    let icon_color = if selected {
        Color32::WHITE
    } else {
        theme::MUTED
    };
    let is_pdf = matches!(icon, NavIcon::Recent | NavIcon::File);
    let show_icon = !is_pdf || file_icons != SidebarFileIcons::None;
    let icon_center = pos2(rect.left() + (indent + 13.0) * scale, rect.center().y);
    if show_icon {
        match icon {
            NavIcon::Recent | NavIcon::File if file_icons == SidebarFileIcons::VsCode => {
                paint_vscode_pdf_icon(painter, icon_center, scale)
            }
            NavIcon::Recent => paint_recent_icon(painter, icon_center, icon_color, scale),
            NavIcon::File => paint_file_icon(painter, icon_center, icon_color, scale),
            NavIcon::Folder => paint_folder_icon(painter, icon_center, icon_color, scale),
        }
    }
    let text_left = rect.left() + (indent + if show_icon { 28.0 } else { 8.0 }) * scale;
    let max_chars = ((rect.right() - text_left - 32.0 * scale) / (7.0 * scale)).max(4.0) as usize;
    painter.text(
        pos2(text_left, rect.center().y),
        Align2::LEFT_CENTER,
        elide(label, max_chars),
        FontId::proportional(13.5 * scale),
        if selected {
            Color32::WHITE
        } else {
            Color32::from_rgb(211, 212, 217)
        },
    );
    let options = sidebar_options_button(ui, rect, &response);
    SidebarRowResponse {
        body: response,
        options,
    }
}

fn max_sidebar_label_chars(
    response: &Response,
    indent: f32,
    file_icons: SidebarFileIcons,
) -> usize {
    let scale = sidebar_visual_scale_from_response(response);
    let text_left = response.rect.left()
        + indent * scale
        + if file_icons == SidebarFileIcons::None {
            8.0 * scale
        } else {
            28.0 * scale
        };
    ((response.rect.right() - text_left - 32.0 * scale) / (7.0 * scale)).max(4.0) as usize
}

fn sidebar_visual_scale_from_response(response: &Response) -> f32 {
    (response.rect.height() / 34.0).clamp(0.1, 4.0)
}

fn sidebar_options_button(ui: &mut egui::Ui, row_rect: Rect, row: &Response) -> Response {
    let scale = sidebar_visual_scale(ui);
    let rect = Rect::from_min_max(
        pos2(
            row_rect.right() - 30.0 * scale,
            row_rect.top() + 3.0 * scale,
        ),
        pos2(
            row_rect.right() - 3.0 * scale,
            row_rect.bottom() - 3.0 * scale,
        ),
    );
    let visible = row.hovered() || ui.rect_contains_pointer(rect);
    ui.put(
        rect,
        egui::Button::new(
            RichText::new(if visible { "..." } else { "" })
                .size(14.0 * scale)
                .color(Color32::from_rgb(205, 208, 219)),
        )
        .frame(false)
        .corner_radius(CornerRadius::same(6)),
    )
    .on_hover_text("More options")
}

fn sidebar_options_popup(
    response: &Response,
    can_reset_name: bool,
    remove_label: Option<&str>,
    include_file_actions: bool,
    pinned: bool,
) -> Option<SidebarMenuAction> {
    let mut action = None;
    egui::Popup::menu(response)
        .width(172.0)
        .frame(
            Frame::new()
                .fill(Color32::from_rgb(29, 31, 37))
                .stroke(Stroke::new(1.0, Color32::from_rgb(61, 64, 75)))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(5)),
        )
        .show(|ui| {
            if include_file_actions
                && popup_menu_row(ui, "Open with...", Color32::from_rgb(225, 227, 234))
            {
                action = Some(SidebarMenuAction::OpenWith);
                ui.close();
            }
            if include_file_actions
                && popup_menu_row(ui, "Open in Files", Color32::from_rgb(225, 227, 234))
            {
                action = Some(SidebarMenuAction::OpenInFiles);
                ui.close();
            }
            if popup_menu_row(
                ui,
                if pinned { "Unpin" } else { "Pin to top" },
                Color32::from_rgb(225, 227, 234),
            ) {
                action = Some(if pinned {
                    SidebarMenuAction::Unpin
                } else {
                    SidebarMenuAction::Pin
                });
                ui.close();
            }
            if popup_menu_row(ui, "Rename display name", Color32::from_rgb(225, 227, 234)) {
                action = Some(SidebarMenuAction::Rename);
                ui.close();
            }
            if can_reset_name
                && popup_menu_row(ui, "Reset display name", Color32::from_rgb(225, 227, 234))
            {
                action = Some(SidebarMenuAction::ResetName);
                ui.close();
            }
            if let Some(remove_label) = remove_label {
                ui.add_space(3.0);
                ui.separator();
                ui.add_space(3.0);
                if popup_menu_row(ui, remove_label, Color32::from_rgb(238, 126, 132)) {
                    action = Some(SidebarMenuAction::Remove);
                    ui.close();
                }
            }
        });
    action
}

fn popup_menu_row(ui: &mut egui::Ui, text: &str, color: Color32) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(7), Color32::from_rgb(44, 47, 56));
    }
    ui.painter().text(
        pos2(rect.left() + 9.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(12.5),
        color,
    );
    response.clicked()
}

fn paint_file_icon(painter: &egui::Painter, center: egui::Pos2, color: Color32, scale: f32) {
    let rect = Rect::from_center_size(center, vec2(8.0, 11.0) * scale);
    painter.rect_stroke(
        rect,
        1.0 * scale,
        Stroke::new(1.2 * scale, color),
        StrokeKind::Inside,
    );
    painter.line_segment(
        [
            pos2(rect.left() + 2.0 * scale, center.y - 2.0 * scale),
            pos2(rect.right() - 2.0 * scale, center.y - 2.0 * scale),
        ],
        Stroke::new(1.0 * scale, color.gamma_multiply(0.8)),
    );
    painter.line_segment(
        [
            pos2(rect.left() + 2.0 * scale, center.y + 2.0 * scale),
            pos2(rect.right() - 2.0 * scale, center.y + 2.0 * scale),
        ],
        Stroke::new(1.0 * scale, color.gamma_multiply(0.8)),
    );
}

/// Compact red PDF glyph inspired by the file association used in VS Code.
fn paint_vscode_pdf_icon(painter: &egui::Painter, center: egui::Pos2, scale: f32) {
    let page = Rect::from_center_size(center, vec2(12.0, 15.0) * scale);
    painter.rect_filled(
        page,
        CornerRadius::same(1),
        Color32::from_rgb(246, 246, 246),
    );
    painter.rect_stroke(
        page,
        CornerRadius::same(1),
        Stroke::new(0.8 * scale, Color32::from_rgb(172, 172, 172)),
        StrokeKind::Inside,
    );
    let band = Rect::from_min_max(
        pos2(page.left(), page.center().y - 3.0 * scale),
        pos2(page.right(), page.center().y + 3.0 * scale),
    );
    painter.rect_filled(band, 0.0, Color32::from_rgb(220, 63, 54));
    painter.text(
        band.center(),
        Align2::CENTER_CENTER,
        "PDF",
        FontId::proportional(5.0 * scale),
        Color32::WHITE,
    );
}

fn paint_recent_icon(painter: &egui::Painter, center: egui::Pos2, color: Color32, scale: f32) {
    let tile = Rect::from_center_size(center, vec2(12.0, 12.0) * scale);
    painter.rect_stroke(
        tile,
        CornerRadius::same(4),
        Stroke::new(1.0 * scale, color.gamma_multiply(0.78)),
        StrokeKind::Inside,
    );
    painter.rect_filled(
        Rect::from_min_max(
            pos2(tile.left() + 3.0 * scale, tile.top() + 3.0 * scale),
            pos2(tile.right() - 3.0 * scale, tile.top() + 5.0 * scale),
        ),
        1.0 * scale,
        color,
    );
    painter.rect_filled(
        Rect::from_min_max(
            pos2(tile.left() + 3.0 * scale, tile.top() + 7.0 * scale),
            pos2(tile.right() - 5.0 * scale, tile.top() + 9.0 * scale),
        ),
        1.0 * scale,
        color.gamma_multiply(0.72),
    );
}

fn paint_folder_icon(painter: &egui::Painter, center: egui::Pos2, color: Color32, scale: f32) {
    let body = Rect::from_min_max(
        pos2(center.x - 8.0 * scale, center.y - 3.0 * scale),
        pos2(center.x + 8.0 * scale, center.y + 6.0 * scale),
    );
    painter.rect_filled(body, CornerRadius::same(3), color.gamma_multiply(0.9));
    painter.rect_filled(
        Rect::from_min_max(
            pos2(center.x - 7.0 * scale, center.y - 6.0 * scale),
            pos2(center.x + 1.0 * scale, center.y),
        ),
        CornerRadius::same(2),
        color,
    );
    painter.line_segment(
        [
            pos2(body.left() + 2.0 * scale, body.top() + 2.0 * scale),
            pos2(body.right() - 2.0 * scale, body.top() + 2.0 * scale),
        ],
        Stroke::new(1.0 * scale, Color32::from_white_alpha(70)),
    );
}

fn sidebar_section_header(ui: &mut egui::Ui, title: &str, subtitle: &str, accent: Color32) {
    let scale = sidebar_visual_scale(ui);
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), 28.0 * scale), Sense::hover());
    ui.painter().text(
        pos2(rect.left(), rect.center().y),
        Align2::LEFT_CENTER,
        title,
        FontId::proportional(11.5 * scale),
        accent,
    );
    ui.painter().text(
        pos2(rect.right(), rect.center().y),
        Align2::RIGHT_CENTER,
        subtitle,
        FontId::proportional(10.5 * scale),
        theme::MUTED,
    );
    ui.painter().line_segment(
        [
            pos2(rect.left(), rect.bottom()),
            pos2(rect.right(), rect.bottom()),
        ],
        Stroke::new(1.0, Color32::from_rgb(39, 42, 49)),
    );
    ui.add_space(3.0);
}

fn search_result_row(ui: &mut egui::Ui, title: &str, excerpt: &str, selected: bool) -> Response {
    let scale = sidebar_visual_scale(ui);
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), 50.0 * scale), Sense::click());
    if selected || response.hovered() {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(5),
            if selected {
                Color32::from_rgb(53, 53, 53)
            } else {
                Color32::from_rgb(42, 42, 42)
            },
        );
    }
    ui.painter().text(
        pos2(rect.left() + 8.0 * scale, rect.top() + 8.0 * scale),
        Align2::LEFT_TOP,
        title,
        FontId::proportional(11.0 * scale),
        theme::MUTED,
    );
    ui.painter().text(
        pos2(rect.left() + 8.0 * scale, rect.top() + 25.0 * scale),
        Align2::LEFT_TOP,
        elide(excerpt, 42),
        FontId::proportional(12.5 * scale),
        Color32::from_rgb(216, 216, 216),
    );
    response
}

fn collapsible_section_header(
    ui: &mut egui::Ui,
    title: &str,
    count: usize,
    expanded: bool,
) -> Response {
    let scale = sidebar_visual_scale(ui);
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), 30.0 * scale), Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(5), Color32::from_rgb(40, 40, 40));
    }
    let x = rect.left() + 7.0;
    let center = rect.center().y;
    let chevron = if expanded {
        [
            pos2(x, center - 2.5),
            pos2(x + 4.0, center + 2.0),
            pos2(x + 8.0, center - 2.5),
        ]
    } else {
        [
            pos2(x + 1.0, center - 4.0),
            pos2(x + 5.0, center),
            pos2(x + 1.0, center + 4.0),
        ]
    };
    ui.painter().add(egui::Shape::line(
        chevron.to_vec(),
        Stroke::new(1.25, theme::MUTED),
    ));
    ui.painter().text(
        pos2(x + 16.0, center),
        Align2::LEFT_CENTER,
        title,
        FontId::proportional(11.5 * scale),
        theme::MUTED,
    );
    ui.painter().text(
        pos2(rect.right() - 6.0, center),
        Align2::RIGHT_CENTER,
        count.to_string(),
        FontId::proportional(10.5 * scale),
        theme::MUTED,
    );
    response.on_hover_text(if expanded {
        "Collapse recents"
    } else {
        "Expand recents"
    })
}

fn elide(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let keep = max_chars.saturating_sub(3);
    format!("{}...", text.chars().take(keep).collect::<String>())
}

fn quantized_render_width(width: f32) -> u16 {
    let clamped = width.round().clamp(256.0, 2600.0) as u32;
    (clamped.div_ceil(128) * 128).min(2600) as u16
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Untitled PDF")
        .to_owned()
}

fn matches_search(path: &Path, root: &Path, search: &str) -> bool {
    let search = search.trim().to_lowercase();
    search.is_empty()
        || path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_lowercase()
            .contains(&search)
}

fn glyph_screen_rect(glyph: &TextGlyph, page_rect: Rect, source_size: [f32; 2]) -> Option<Rect> {
    let bounds = glyph.bounds?;
    let rect = pdf_bounds_screen_rect(bounds, page_rect, source_size);
    (rect.width() >= 0.1 && rect.height() >= 0.1).then_some(rect)
}

fn pdf_bounds_screen_rect(bounds: [f32; 4], page_rect: Rect, source_size: [f32; 2]) -> Rect {
    let [left, bottom, right, top] = bounds;
    let source_width = source_size[0].max(1.0);
    let source_height = source_size[1].max(1.0);
    let x1 = page_rect.left() + left / source_width * page_rect.width();
    let x2 = page_rect.left() + right / source_width * page_rect.width();
    let y1 = page_rect.top() + (source_height - top) / source_height * page_rect.height();
    let y2 = page_rect.top() + (source_height - bottom) / source_height * page_rect.height();
    Rect::from_min_max(pos2(x1.min(x2), y1.min(y2)), pos2(x1.max(x2), y1.max(y2)))
}

fn is_safe_external_uri(uri: &str) -> bool {
    let lower = uri.trim().to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:")
}

fn selected_glyph_text(glyphs: &[TextGlyph], anchor: usize, focus: usize) -> Option<String> {
    let start = anchor.min(focus);
    let end = anchor.max(focus);
    let text = glyphs
        .get(start..=end)?
        .iter()
        .map(|glyph| glyph.text.as_str())
        .collect::<String>();
    (!text.is_empty()).then_some(text)
}

fn nearest_glyph(
    glyphs: &[TextGlyph],
    page_rect: Rect,
    source_size: [f32; 2],
    pointer: egui::Pos2,
) -> Option<usize> {
    glyphs
        .iter()
        .enumerate()
        .filter_map(|(index, glyph)| {
            let rect = glyph_screen_rect(glyph, page_rect, source_size)?;
            let dx = if pointer.x < rect.left() {
                rect.left() - pointer.x
            } else if pointer.x > rect.right() {
                pointer.x - rect.right()
            } else {
                0.0
            };
            let dy = if pointer.y < rect.top() {
                rect.top() - pointer.y
            } else if pointer.y > rect.bottom() {
                pointer.y - rect.bottom()
            } else {
                0.0
            };
            Some((index, dx * dx + dy * dy))
        })
        .min_by(|(_, left), (_, right)| left.total_cmp(right))
        .and_then(|(index, distance)| (distance <= 8.0 * 8.0).then_some(index))
}

fn paint_search_hits(
    painter: &egui::Painter,
    glyphs: &[TextGlyph],
    query: &str,
    page_rect: Rect,
    source_size: [f32; 2],
) {
    let mut characters = Vec::new();
    let mut glyph_for_character = Vec::new();
    for (glyph_index, glyph) in glyphs.iter().enumerate() {
        for character in glyph.text.to_lowercase().chars() {
            characters.push(character);
            glyph_for_character.push(glyph_index);
        }
    }
    let needle = query.to_lowercase().chars().collect::<Vec<_>>();
    if needle.is_empty() || needle.len() > characters.len() {
        return;
    }
    for start in 0..=characters.len() - needle.len() {
        if characters[start..start + needle.len()] != needle {
            continue;
        }
        let first = glyph_for_character[start];
        let last = glyph_for_character[start + needle.len() - 1];
        for glyph in &glyphs[first..=last] {
            if let Some(bounds) = glyph.bounds {
                painter.rect_filled(
                    pdf_bounds_screen_rect(bounds, page_rect, source_size),
                    1.0,
                    Color32::from_rgba_unmultiplied(255, 196, 54, 92),
                );
            }
        }
    }
}

fn paint_text_selection(
    painter: &egui::Painter,
    glyphs: &[TextGlyph],
    first: usize,
    last: usize,
    page_rect: Rect,
    source_size: [f32; 2],
) {
    let Some(selected) = glyphs.get(first..=last) else {
        return;
    };
    for glyph in selected {
        if let Some(rect) = glyph_screen_rect(glyph, page_rect, source_size) {
            painter.rect_filled(
                rect.expand2(vec2(0.6, 0.4)),
                0.5,
                Color32::from_rgba_unmultiplied(52, 132, 255, 92),
            );
        }
    }
}

fn text_selection_screen_rect(
    glyphs: &[TextGlyph],
    first: usize,
    last: usize,
    page_rect: Rect,
    source_size: [f32; 2],
) -> Option<Rect> {
    glyphs
        .get(first..=last)?
        .iter()
        .filter_map(|glyph| glyph_screen_rect(glyph, page_rect, source_size))
        .reduce(Rect::union)
}

/// During an ongoing drag, the pointer may be in whitespace or even beyond
/// the visible page. Keep choosing the closest glyph instead of dropping the
/// selection when `nearest_glyph`'s ordinary click tolerance is exceeded.
fn nearest_drag_glyph(
    glyphs: &[TextGlyph],
    page_rect: Rect,
    source_size: [f32; 2],
    pointer: egui::Pos2,
) -> Option<usize> {
    glyphs
        .iter()
        .enumerate()
        .filter_map(|(index, glyph)| {
            let rect = glyph_screen_rect(glyph, page_rect, source_size)?;
            let dx = (rect.left() - pointer.x).max(0.0) + (pointer.x - rect.right()).max(0.0);
            let dy = (rect.top() - pointer.y).max(0.0) + (pointer.y - rect.bottom()).max(0.0);
            Some((index, dx * dx + dy * dy))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(index, _)| index)
}

/// Positive values move PDF content down, negative values move it up.
/// Start gently inside a 48-point edge zone and accelerate as the pointer
/// approaches (or passes) the boundary. Works in both directions.
fn selection_edge_scroll(pointer_y: f32, viewport: Rect) -> f32 {
    const EDGE: f32 = 48.0;
    const MAX_SPEED: f32 = 28.0;
    let above = (viewport.top() + EDGE - pointer_y).max(0.0) / EDGE;
    let below = (pointer_y - (viewport.bottom() - EDGE)).max(0.0) / EDGE;
    above.min(2.0) * MAX_SPEED - below.min(2.0) * MAX_SPEED
}

fn selection_action_bar(ctx: &egui::Context, selection_rect: Rect) -> Option<SelectionAction> {
    let mut action = None;
    egui::Area::new(egui::Id::new("text-selection-actions"))
        .order(egui::Order::Tooltip)
        .fixed_pos(pos2(
            selection_rect.center().x - 70.0,
            selection_rect.top() - 40.0,
        ))
        .show(ctx, |ui| {
            Frame::new()
                .fill(Color32::from_rgb(31, 31, 31))
                .stroke(Stroke::new(1.0, Color32::from_rgb(70, 70, 70)))
                .corner_radius(CornerRadius::same(7))
                .inner_margin(Margin::symmetric(5, 4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui.small_button("Copy").clicked() {
                            action = Some(SelectionAction::Copy);
                        }
                        if ui.small_button("Highlight").clicked() {
                            action = Some(SelectionAction::Highlight);
                        }
                    });
                });
        });
    action
}

fn paint_eraser_cursor(painter: &egui::Painter, pointer: egui::Pos2) {
    let points = vec![
        pointer + vec2(-7.0, 4.0),
        pointer + vec2(-2.0, -6.0),
        pointer + vec2(8.0, -1.0),
        pointer + vec2(3.0, 9.0),
    ];
    painter.add(egui::Shape::convex_polygon(
        points,
        Color32::from_rgb(226, 143, 151),
        Stroke::new(1.2, Color32::from_rgb(245, 221, 224)),
    ));
    painter.line_segment(
        [pointer + vec2(-4.5, -1.0), pointer + vec2(5.5, 4.0)],
        Stroke::new(1.0, Color32::from_rgb(116, 82, 88)),
    );
}

fn show_comment_card(
    ctx: &egui::Context,
    annotation: &Annotation,
    page_rect: Rect,
    source_size: [f32; 2],
    pinned: bool,
) -> bool {
    let AnnotationKind::Note { position, text, .. } = &annotation.kind else {
        return false;
    };
    let anchor = pdf_point_to_screen(*position, page_rect, source_size);
    let mut close = false;
    egui::Area::new(egui::Id::new(("comment-card", annotation.id)))
        .order(egui::Order::Tooltip)
        .fixed_pos(anchor + vec2(14.0, 10.0))
        .show(ctx, |ui| {
            Frame::new()
                .fill(Color32::from_rgb(32, 32, 32))
                .stroke(Stroke::new(1.0, Color32::from_rgb(72, 72, 72)))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(9))
                .show(ui, |ui| {
                    ui.set_max_width(260.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Comment")
                                .size(11.0)
                                .strong()
                                .color(theme::MUTED),
                        );
                        if pinned {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.add(egui::Button::new("×").frame(false)).clicked() {
                                    close = true;
                                }
                            });
                        }
                    });
                    ui.add_space(3.0);
                    ui.label(
                        RichText::new(text)
                            .size(13.0)
                            .color(Color32::from_rgb(225, 225, 225)),
                    );
                });
        });
    close
}

fn tool_button(ui: &mut egui::Ui, mode: &mut ToolMode, value: ToolMode, label: &str, help: &str) {
    let selected = *mode == value;
    if ui
        .selectable_label(selected, label)
        .on_hover_text(help)
        .clicked()
    {
        *mode = if selected && value != ToolMode::Select {
            ToolMode::Select
        } else {
            value
        };
    }
}

#[derive(Clone, Copy)]
enum ToolbarIcon {
    Highlight,
    Pen,
    Eraser,
    Comment,
    Ocr,
    Math,
    Undo,
    Redo,
    Print,
    Export,
    Save,
    OpenWith,
    Folder,
    Settings,
    CollapseToolbar,
    Reader,
    Present,
}

#[derive(Clone, Copy)]
enum ModeControlIcon {
    Exit,
    ShowToolbar,
}

fn mini_mode_icon_button(ui: &mut egui::Ui, icon: ModeControlIcon, help: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(21.0, 21.0), Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(5), Color32::from_rgb(54, 54, 54));
    }
    let color = Color32::from_rgb(214, 214, 214);
    let center = rect.center();
    match icon {
        ModeControlIcon::Exit => {
            ui.painter().line_segment(
                [center + vec2(-4.0, -4.0), center + vec2(4.0, 4.0)],
                Stroke::new(1.45, color),
            );
            ui.painter().line_segment(
                [center + vec2(4.0, -4.0), center + vec2(-4.0, 4.0)],
                Stroke::new(1.45, color),
            );
        }
        ModeControlIcon::ShowToolbar => {
            ui.painter().line_segment(
                [center + vec2(-5.0, -4.0), center + vec2(5.0, -4.0)],
                Stroke::new(1.3, color),
            );
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    center + vec2(-4.0, 0.0),
                    center + vec2(4.0, 0.0),
                    center + vec2(0.0, 4.5),
                ],
                color,
                Stroke::NONE,
            ));
        }
    }
    response.on_hover_text(help)
}

fn compact_text_button(ui: &mut egui::Ui, text: &str, help: &str) -> Response {
    ui.add_sized(
        [22.0, 20.0],
        egui::Button::new(RichText::new(text).size(13.0)).frame(false),
    )
    .on_hover_text(help)
}

fn tool_icon_button(
    ui: &mut egui::Ui,
    mode: &mut ToolMode,
    value: ToolMode,
    icon: ToolbarIcon,
    help: &str,
) -> Response {
    let selected = *mode == value;
    let response = icon_button(ui, icon, selected, true, help);
    if response.clicked() {
        *mode = if selected { ToolMode::Select } else { value };
    }
    response
}

fn icon_button(
    ui: &mut egui::Ui,
    icon: ToolbarIcon,
    selected: bool,
    enabled: bool,
    help: &str,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(24.0, 20.0), Sense::click());
    if selected || response.hovered() {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(5),
            if selected {
                Color32::from_rgb(65, 65, 65)
            } else {
                Color32::from_rgb(45, 45, 45)
            },
        );
    }
    let color = if enabled {
        Color32::from_rgb(205, 205, 205)
    } else {
        Color32::from_rgb(88, 88, 88)
    };
    paint_toolbar_icon(ui.painter(), rect.center(), icon, color);
    response.on_hover_text(help)
}

fn paint_toolbar_icon(
    painter: &egui::Painter,
    center: egui::Pos2,
    icon: ToolbarIcon,
    color: Color32,
) {
    let stroke = Stroke::new(1.45, color);
    match icon {
        ToolbarIcon::Highlight => {
            // Conventional chisel-tip highlighter with a bright ink edge.
            let body = vec![
                center + vec2(-6.5, 3.0),
                center + vec2(1.5, -5.0),
                center + vec2(6.5, 0.0),
                center + vec2(-1.5, 8.0),
            ];
            painter.add(egui::Shape::convex_polygon(
                body.clone(),
                Color32::from_rgb(224, 224, 224),
                Stroke::new(1.1, Color32::from_rgb(34, 34, 34)),
            ));
            painter.line_segment(
                [center + vec2(-4.3, 5.0), center + vec2(3.8, -3.1)],
                Stroke::new(1.25, Color32::from_rgb(105, 105, 105)),
            );
            painter.line_segment(
                [center + vec2(-7.0, 8.0), center + vec2(7.0, 8.0)],
                Stroke::new(2.7, Color32::from_rgb(255, 211, 59)),
            );
        }
        ToolbarIcon::Pen => {
            // Fountain-pen nib rather than a generic diagonal slash.
            let nib = vec![
                center + vec2(-6.5, 6.5),
                center + vec2(-2.5, -6.0),
                center + vec2(6.0, 2.5),
            ];
            painter.add(egui::Shape::convex_polygon(nib, color, Stroke::NONE));
            painter.line_segment(
                [center + vec2(-4.5, 4.5), center + vec2(3.0, -3.0)],
                Stroke::new(1.15, Color32::from_rgb(46, 46, 46)),
            );
            painter.circle_filled(
                center + vec2(-3.8, 3.8),
                1.15,
                Color32::from_rgb(46, 46, 46),
            );
        }
        ToolbarIcon::Eraser => {
            let white_half = vec![
                center + vec2(-7.0, 1.0),
                center + vec2(-2.0, -6.0),
                center + vec2(2.2, -1.8),
                center + vec2(-2.8, 6.2),
            ];
            let dark_half = vec![
                center + vec2(2.2, -1.8),
                center + vec2(7.0, 2.9),
                center + vec2(1.8, 7.0),
                center + vec2(-2.8, 6.2),
            ];
            painter.add(egui::Shape::convex_polygon(
                white_half,
                Color32::from_rgb(238, 238, 238),
                Stroke::NONE,
            ));
            painter.add(egui::Shape::convex_polygon(
                dark_half,
                Color32::from_rgb(196, 201, 208),
                Stroke::NONE,
            ));
            painter.add(egui::Shape::closed_line(
                vec![
                    center + vec2(-7.0, 1.5),
                    center + vec2(-2.0, -6.0),
                    center + vec2(7.0, 2.5),
                    center + vec2(2.0, 7.0),
                    center + vec2(-7.0, 1.5),
                ],
                Stroke::new(1.0, color.gamma_multiply(0.7)),
            ));
        }
        ToolbarIcon::Comment => {
            let bubble = Rect::from_center_size(center - vec2(0.0, 1.5), vec2(15.0, 11.0));
            painter.rect_stroke(bubble, 3.5, stroke, StrokeKind::Inside);
            painter.line_segment(
                [
                    bubble.left_bottom() + vec2(4.0, 0.0),
                    bubble.left_bottom() + vec2(1.5, 4.0),
                ],
                stroke,
            );
            for x in [-3.5, 0.0, 3.5] {
                painter.circle_filled(center + vec2(x, -1.5), 0.85, color);
            }
        }
        ToolbarIcon::Ocr => {
            painter.text(
                center,
                Align2::CENTER_CENTER,
                "OCR",
                FontId::monospace(8.5),
                color,
            );
        }
        ToolbarIcon::Math => {
            painter.text(
                center,
                Align2::CENTER_CENTER,
                "fx",
                FontId::proportional(11.0),
                color,
            );
        }
        ToolbarIcon::Undo | ToolbarIcon::Redo => {
            let flip = if matches!(icon, ToolbarIcon::Undo) {
                1.0
            } else {
                -1.0
            };
            let u_turn = vec![
                center + vec2(-6.7 * flip, -2.8),
                center + vec2(-2.0 * flip, -2.8),
                center + vec2(3.0 * flip, -2.8),
                center + vec2(6.0 * flip, 0.0),
                center + vec2(5.6 * flip, 3.9),
                center + vec2(2.2 * flip, 6.0),
                center + vec2(-1.5 * flip, 5.4),
            ];
            painter.add(egui::Shape::line(u_turn, Stroke::new(1.7, color)));
            let arrow_tip = center + vec2(-7.0 * flip, -2.8);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    arrow_tip,
                    arrow_tip + vec2(4.2 * flip, -3.6),
                    arrow_tip + vec2(4.2 * flip, 3.6),
                ],
                color,
                Stroke::NONE,
            ));
        }
        ToolbarIcon::Print => {
            painter.rect_stroke(
                Rect::from_center_size(center + vec2(0.0, -4.0), vec2(11.0, 8.0)),
                0.0,
                stroke,
                StrokeKind::Inside,
            );
            painter.rect_stroke(
                Rect::from_center_size(center + vec2(0.0, 4.0), vec2(15.0, 8.0)),
                2.0,
                stroke,
                StrokeKind::Inside,
            );
        }
        ToolbarIcon::Export => {
            painter.rect_stroke(
                Rect::from_center_size(center + vec2(0.0, 3.0), vec2(13.0, 10.0)),
                1.0,
                stroke,
                StrokeKind::Inside,
            );
            painter.line_segment([center + vec2(0.0, 3.0), center + vec2(0.0, -7.0)], stroke);
            painter.line_segment(
                [center + vec2(0.0, -7.0), center + vec2(-3.0, -4.0)],
                stroke,
            );
            painter.line_segment([center + vec2(0.0, -7.0), center + vec2(3.0, -4.0)], stroke);
        }
        ToolbarIcon::Save => {
            let body = Rect::from_center_size(center, vec2(14.0, 14.0));
            painter.rect_stroke(body, 1.0, stroke, StrokeKind::Inside);
            painter.rect_stroke(
                Rect::from_min_max(
                    body.left_top() + vec2(3.0, 0.0),
                    body.right_top() + vec2(-2.0, 5.0),
                ),
                0.0,
                stroke,
                StrokeKind::Inside,
            );
            painter.rect_stroke(
                Rect::from_min_max(
                    body.left_bottom() + vec2(3.0, -6.0),
                    body.right_bottom() + vec2(-3.0, -2.0),
                ),
                0.0,
                stroke,
                StrokeKind::Inside,
            );
        }
        ToolbarIcon::OpenWith => {
            painter.rect_stroke(
                Rect::from_center_size(center + vec2(-2.0, 2.0), vec2(12.0, 12.0)),
                2.0,
                stroke,
                StrokeKind::Inside,
            );
            painter.line_segment([center + vec2(0.0, 0.0), center + vec2(7.0, -7.0)], stroke);
            painter.line_segment([center + vec2(7.0, -7.0), center + vec2(3.0, -7.0)], stroke);
            painter.line_segment([center + vec2(7.0, -7.0), center + vec2(7.0, -3.0)], stroke);
        }
        ToolbarIcon::Folder => {
            let body = Rect::from_min_max(center + vec2(-8.0, -3.0), center + vec2(8.0, 6.0));
            painter.rect_stroke(body, 2.0, stroke, StrokeKind::Inside);
            painter.line_segment(
                [center + vec2(-7.0, -3.0), center + vec2(-4.0, -6.0)],
                stroke,
            );
            painter.line_segment(
                [center + vec2(-4.0, -6.0), center + vec2(1.0, -6.0)],
                stroke,
            );
            painter.line_segment([center + vec2(1.0, -6.0), center + vec2(3.0, -3.0)], stroke);
        }
        ToolbarIcon::Settings => {
            // A continuous toothed cog reads as Settings at small sizes more
            // clearly than the previous sunburst of disconnected lines.
            let teeth = (0..16)
                .map(|step| {
                    let angle = step as f32 / 16.0 * std::f32::consts::TAU;
                    let radius = if step % 2 == 0 { 8.0 } else { 6.35 };
                    center + vec2(angle.cos() * radius, angle.sin() * radius)
                })
                .collect::<Vec<_>>();
            painter.add(egui::Shape::convex_polygon(teeth, color, Stroke::NONE));
            painter.circle_filled(center, 4.35, theme::PANEL_BG);
            painter.circle_stroke(center, 4.35, Stroke::new(1.05, color));
            painter.circle_filled(center, 1.5, color);
        }
        ToolbarIcon::CollapseToolbar => {
            painter.line_segment([center + vec2(-7.0, 5.0), center + vec2(7.0, 5.0)], stroke);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    center + vec2(-4.0, 0.0),
                    center + vec2(4.0, 0.0),
                    center + vec2(0.0, -4.5),
                ],
                color,
                Stroke::NONE,
            ));
        }
        ToolbarIcon::Reader => {
            let page = Rect::from_center_size(center, vec2(12.0, 15.0));
            painter.rect_stroke(page, 1.0, stroke, StrokeKind::Inside);
            painter.line_segment(
                [
                    page.left_top() + vec2(3.0, 4.0),
                    page.right_top() + vec2(-3.0, 4.0),
                ],
                Stroke::new(1.1, color),
            );
            painter.line_segment(
                [
                    page.left_top() + vec2(3.0, 8.0),
                    page.right_top() + vec2(-3.0, 8.0),
                ],
                Stroke::new(1.1, color),
            );
        }
        ToolbarIcon::Present => {
            let screen = Rect::from_center_size(center, vec2(15.0, 11.0));
            painter.rect_stroke(screen, 1.0, stroke, StrokeKind::Inside);
            painter.line_segment([center + vec2(-3.0, 7.0), center + vec2(3.0, 7.0)], stroke);
            painter.line_segment([center + vec2(0.0, 4.0), center + vec2(0.0, 7.0)], stroke);
        }
    }
}

fn path_breadcrumbs(ui: &mut egui::Ui, path: Option<&Path>) -> Option<BreadcrumbAction> {
    let Some(path) = path else {
        ui.label(
            RichText::new("No document open")
                .size(13.0)
                .color(theme::MUTED),
        );
        return None;
    };

    let full_path = path.display().to_string();
    let mut action = None;
    let mut cumulative = PathBuf::from("/");
    let mut parts = path.components().peekable();
    while let Some(component) = parts.next() {
        let label = match component {
            std::path::Component::RootDir => continue,
            _ => component.as_os_str().to_string_lossy().into_owned(),
        };
        cumulative.push(component.as_os_str());
        let is_last = parts.peek().is_none();
        let response = ui
            .add(
                egui::Button::new(
                    RichText::new(label)
                        .size(13.0)
                        .color(Color32::from_rgb(177, 177, 177)),
                )
                .frame(false)
                .corner_radius(CornerRadius::same(4)),
            )
            .on_hover_text(if is_last {
                format!("Copy path\n{full_path}")
            } else {
                format!("Browse PDFs in {}", cumulative.display())
            });
        if response.clicked() {
            action = Some(if is_last {
                BreadcrumbAction::Copy(path.to_path_buf())
            } else {
                BreadcrumbAction::Browse(cumulative.clone())
            });
        }
        if !is_last {
            ui.label(
                RichText::new("/")
                    .size(13.0)
                    .color(Color32::from_rgb(92, 92, 92)),
            );
        }
    }
    action
}

fn chrome_zoom(document_zoom: f32, ratio: f32) -> f32 {
    (document_zoom * ratio).clamp(0.28, 5.0)
}

/// Compute the *absolute* ScrollArea offset that preserves a PDF point
/// beneath the gesture cursor across a scale change. A page's vertical content
/// coordinate is the sum of prior page heights plus its relative position;
/// page gaps cancel because they are not zoomed. Horizontally, each page is
/// centered within the (possibly wider) document canvas.
#[allow(clippy::too_many_arguments)]
fn pinch_anchored_offset(
    previous_offset: egui::Vec2,
    anchor: PinchAnchor,
    page_sizes: &[[f32; 2]],
    previous_zoom: f32,
    new_zoom: f32,
    previous_canvas_width: f32,
    new_canvas_width: f32,
    presentation_mode: bool,
) -> egui::Vec2 {
    let Some(page_size) = page_sizes.get(anchor.page) else {
        return previous_offset;
    };
    let delta_zoom = new_zoom - previous_zoom;
    let preceding_height: f32 = if presentation_mode {
        0.0
    } else {
        page_sizes[..anchor.page].iter().map(|size| size[1]).sum()
    };
    let delta_y = (preceding_height + page_size[1] * anchor.fraction.y) * delta_zoom;
    let delta_x = (new_canvas_width - previous_canvas_width) * 0.5
        + (anchor.fraction.x - 0.5) * page_size[0] * delta_zoom;
    (previous_offset + vec2(delta_x, delta_y)).max(egui::Vec2::ZERO)
}

/// Return only zoom gestures produced by a physical trackpad/touch surface.
///
/// Wayland and some X11 drivers emit `Event::Zoom` directly. On X11, precision
/// touchpad pinches are commonly represented as point-granularity wheel events
/// with a synthetic Ctrl modifier. A real mouse wheel uses line granularity, so
/// excluding it here keeps Ctrl+mouse-wheel out of this PDF-only gesture path.
fn trackpad_pinch_factor(events: &[egui::Event]) -> f32 {
    let mut native_factor = 1.0_f32;
    let mut precision_scroll = 0.0_f32;

    for event in events {
        match event {
            egui::Event::Zoom(factor) => native_factor *= *factor,
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta,
                modifiers,
                ..
            } if modifiers.ctrl || modifiers.command => {
                precision_scroll += delta.x + delta.y;
            }
            _ => {}
        }
    }

    if (native_factor - 1.0).abs() > f32::EPSILON {
        native_factor
    } else if precision_scroll.abs() > f32::EPSILON {
        (precision_scroll / 200.0).exp()
    } else {
        1.0
    }
}

fn screen_point_to_pdf(point: egui::Pos2, page_rect: Rect, source_size: [f32; 2]) -> [f32; 2] {
    let x = ((point.x - page_rect.left()) / page_rect.width()).clamp(0.0, 1.0) * source_size[0];
    let y_from_top =
        ((point.y - page_rect.top()) / page_rect.height()).clamp(0.0, 1.0) * source_size[1];
    [x, source_size[1] - y_from_top]
}

fn screen_rect_to_pdf(rect: Rect, page_rect: Rect, source_size: [f32; 2]) -> [f32; 4] {
    let bottom_left = screen_point_to_pdf(pos2(rect.left(), rect.bottom()), page_rect, source_size);
    let top_right = screen_point_to_pdf(pos2(rect.right(), rect.top()), page_rect, source_size);
    [bottom_left[0], bottom_left[1], top_right[0], top_right[1]]
}

fn pdf_point_to_screen(point: [f32; 2], page_rect: Rect, source_size: [f32; 2]) -> egui::Pos2 {
    pos2(
        page_rect.left() + point[0] / source_size[0].max(1.0) * page_rect.width(),
        page_rect.top()
            + (source_size[1] - point[1]) / source_size[1].max(1.0) * page_rect.height(),
    )
}

fn annotation_color(color: [u8; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3])
}

fn paint_annotations(
    painter: &egui::Painter,
    annotations: &[Annotation],
    page_rect: Rect,
    source_size: [f32; 2],
) {
    // Highlights are always the bottom annotation layer so ink and comments remain legible.
    for annotation in annotations {
        if let AnnotationKind::Highlight { rects, color } = &annotation.kind {
            for bounds in rects {
                painter.rect_filled(
                    pdf_bounds_screen_rect(*bounds, page_rect, source_size).expand2(vec2(0.5, 0.3)),
                    1.0,
                    annotation_color(*color),
                );
            }
        }
    }
    for annotation in annotations {
        if let AnnotationKind::Ink {
            points,
            color,
            width,
        } = &annotation.kind
        {
            let screen = points
                .iter()
                .map(|point| pdf_point_to_screen(*point, page_rect, source_size))
                .collect::<Vec<_>>();
            if screen.len() > 1 {
                painter.add(egui::Shape::line(
                    screen,
                    Stroke::new(*width, annotation_color(*color)),
                ));
            }
        }
    }
    for annotation in annotations {
        match &annotation.kind {
            AnnotationKind::TextBox { rect, text, color } => {
                let screen = pdf_bounds_screen_rect(*rect, page_rect, source_size);
                painter.rect_stroke(
                    screen,
                    CornerRadius::same(2),
                    Stroke::new(1.0, annotation_color(*color)),
                    StrokeKind::Inside,
                );
                painter.text(
                    screen.left_top() + vec2(4.0, 4.0),
                    Align2::LEFT_TOP,
                    text,
                    FontId::proportional(12.5),
                    annotation_color(*color),
                );
            }
            AnnotationKind::Shape {
                rect,
                color,
                width,
                ellipse,
            } => {
                let screen = pdf_bounds_screen_rect(*rect, page_rect, source_size);
                let stroke = Stroke::new(*width, annotation_color(*color));
                if *ellipse {
                    painter.circle_stroke(
                        screen.center(),
                        screen.width().min(screen.height()) * 0.5,
                        stroke,
                    );
                } else {
                    painter.rect_stroke(screen, 0.0, stroke, StrokeKind::Inside);
                }
            }
            AnnotationKind::Stamp {
                position,
                text,
                color,
            } => {
                let anchor = pdf_point_to_screen(*position, page_rect, source_size);
                let rect = Rect::from_min_size(
                    anchor,
                    vec2((text.len() as f32 * 8.0 + 16.0).max(72.0), 28.0),
                );
                painter.rect_stroke(
                    rect,
                    CornerRadius::same(3),
                    Stroke::new(2.0, annotation_color(*color)),
                    StrokeKind::Inside,
                );
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    text.to_uppercase(),
                    FontId::proportional(12.0),
                    annotation_color(*color),
                );
            }
            AnnotationKind::Attachment { position, path } => {
                let center = pdf_point_to_screen(*position, page_rect, source_size);
                let color = Color32::from_rgb(64, 112, 196);
                painter.rect_filled(
                    Rect::from_center_size(center, vec2(18.0, 18.0)),
                    CornerRadius::same(4),
                    color,
                );
                painter.text(
                    center,
                    Align2::CENTER_CENTER,
                    "+",
                    FontId::proportional(14.0),
                    Color32::WHITE,
                );
                painter.text(
                    pos2(center.x + 12.0, center.y),
                    Align2::LEFT_CENTER,
                    elide(&file_name(path), 24),
                    FontId::proportional(11.0),
                    Color32::from_rgb(38, 38, 42),
                );
            }
            _ => {}
        }
    }
    // Comments are deliberately painted last, above highlights and drawings.
    for annotation in annotations {
        if let AnnotationKind::Note {
            position, color, ..
        } = &annotation.kind
        {
            let center = pdf_point_to_screen(*position, page_rect, source_size);
            let bubble = Rect::from_center_size(center, vec2(18.0, 14.0));
            let fill = annotation_color(*color);
            painter.rect_filled(bubble, CornerRadius::same(5), fill);
            painter.rect_stroke(
                bubble,
                CornerRadius::same(5),
                Stroke::new(1.0, Color32::from_black_alpha(80)),
                StrokeKind::Inside,
            );
            painter.add(egui::Shape::convex_polygon(
                vec![
                    pos2(bubble.left() + 4.0, bubble.bottom() - 0.5),
                    pos2(bubble.left() + 7.5, bubble.bottom() - 0.5),
                    pos2(bubble.left() + 4.0, bubble.bottom() + 4.0),
                ],
                fill,
                Stroke::NONE,
            ));
            for offset in [-4.0, 0.0, 4.0] {
                painter.circle_filled(
                    pos2(center.x + offset, center.y),
                    1.2,
                    Color32::from_rgb(38, 38, 42),
                );
            }
        }
    }
}

fn annotation_context_menu(
    ui: &mut egui::Ui,
    annotations: &[Annotation],
    page_rect: Rect,
    source_size: [f32; 2],
    erase_on_click: bool,
) -> AnnotationUiResult {
    let mut action = None;
    let mut hovered_comment = None;
    let mut clicked_comment = None;
    for annotation in annotations {
        let Some(rect) = annotation_screen_rect(annotation, page_rect, source_size) else {
            continue;
        };
        let response = ui.interact(
            rect,
            ui.id().with(("annotation-context", annotation.id)),
            Sense::click(),
        );
        if erase_on_click && response.clicked() {
            action = Some(AnnotationContextAction::Delete(annotation.id));
        }
        if matches!(&annotation.kind, AnnotationKind::Note { .. }) {
            if response.hovered() {
                hovered_comment = Some(annotation.id);
            }
            if response.clicked() && !erase_on_click {
                clicked_comment = Some(annotation.id);
            }
        }
        let child_comment = annotations.iter().find_map(|candidate| {
            matches!(
                &candidate.kind,
                AnnotationKind::Note {
                    parent: Some(parent),
                    ..
                } if *parent == annotation.id
            )
            .then_some(candidate.id)
        });
        egui::Popup::context_menu(&response)
            .width(184.0)
            .frame(
                Frame::new()
                    .fill(Color32::from_rgb(29, 29, 29))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(65, 65, 65)))
                    .corner_radius(CornerRadius::same(7))
                    .inner_margin(Margin::same(5)),
            )
            .show(|ui| {
                let (kind, delete_label) = match &annotation.kind {
                    AnnotationKind::Highlight { .. } => ("HIGHLIGHT", "Delete highlight"),
                    AnnotationKind::Ink { .. } => ("DRAWING", "Delete drawing"),
                    AnnotationKind::Note { .. } => ("COMMENT", "Delete comment"),
                    AnnotationKind::TextBox { .. } => ("TEXT BOX", "Delete text box"),
                    AnnotationKind::Shape { .. } => ("SHAPE", "Delete shape"),
                    AnnotationKind::Stamp { .. } => ("STAMP", "Delete stamp"),
                    AnnotationKind::Attachment { .. } => ("ATTACHMENT", "Delete attachment"),
                };
                ui.label(RichText::new(kind).size(10.5).strong().color(theme::MUTED));
                ui.add_space(3.0);
                if matches!(&annotation.kind, AnnotationKind::Highlight { .. }) {
                    let label = if child_comment.is_some() {
                        "Edit comment"
                    } else {
                        "Add comment"
                    };
                    if popup_menu_row(ui, label, Color32::from_rgb(225, 227, 234)) {
                        if let Some(comment_id) = child_comment {
                            action = Some(AnnotationContextAction::EditComment(comment_id));
                        } else if let Some(position) = annotation_comment_position(annotation) {
                            action = Some(AnnotationContextAction::AddComment {
                                page: annotation.page,
                                position,
                                parent: Some(annotation.id),
                            });
                        }
                        ui.close();
                    }
                }
                if matches!(&annotation.kind, AnnotationKind::Note { .. })
                    && popup_menu_row(ui, "Edit comment", Color32::from_rgb(225, 227, 234))
                {
                    action = Some(AnnotationContextAction::EditComment(annotation.id));
                    ui.close();
                }
                if popup_menu_row(ui, delete_label, Color32::from_rgb(238, 126, 132)) {
                    action = Some(AnnotationContextAction::Delete(annotation.id));
                    ui.close();
                }
            });
    }
    AnnotationUiResult {
        action,
        hovered_comment,
        clicked_comment,
    }
}

fn annotation_screen_rect(
    annotation: &Annotation,
    page_rect: Rect,
    source_size: [f32; 2],
) -> Option<Rect> {
    match &annotation.kind {
        AnnotationKind::Highlight { rects, .. } => rects
            .iter()
            .map(|bounds| pdf_bounds_screen_rect(*bounds, page_rect, source_size))
            .reduce(Rect::union)
            .map(|rect| rect.expand(5.0)),
        AnnotationKind::Ink { points, .. } => {
            let mut points = points
                .iter()
                .map(|point| pdf_point_to_screen(*point, page_rect, source_size));
            let first = points.next()?;
            let mut rect = Rect::from_min_max(first, first);
            for point in points {
                rect.extend_with(point);
            }
            Some(rect.expand(9.0))
        }
        AnnotationKind::Note { position, .. } => {
            let center = pdf_point_to_screen(*position, page_rect, source_size);
            Some(Rect::from_center_size(center, vec2(24.0, 24.0)))
        }
        AnnotationKind::TextBox { rect, .. } | AnnotationKind::Shape { rect, .. } => {
            Some(pdf_bounds_screen_rect(*rect, page_rect, source_size).expand(5.0))
        }
        AnnotationKind::Stamp { position, text, .. } => {
            let anchor = pdf_point_to_screen(*position, page_rect, source_size);
            Some(Rect::from_min_size(
                anchor,
                vec2((text.len() as f32 * 8.0 + 16.0).max(72.0), 28.0),
            ))
        }
        AnnotationKind::Attachment { position, .. } => {
            let center = pdf_point_to_screen(*position, page_rect, source_size);
            Some(Rect::from_center_size(center, vec2(26.0, 26.0)))
        }
    }
}

fn annotation_comment_position(annotation: &Annotation) -> Option<[f32; 2]> {
    let AnnotationKind::Highlight { rects, .. } = &annotation.kind else {
        return None;
    };
    let right = rects.iter().map(|rect| rect[2]).reduce(f32::max)?;
    let top = rects.iter().map(|rect| rect[3]).reduce(f32::max)?;
    Some([right + 5.0, top])
}

fn paint_ink_preview(
    painter: &egui::Painter,
    points: &[[f32; 2]],
    page_rect: Rect,
    source_size: [f32; 2],
    color: Color32,
) {
    let points = points
        .iter()
        .map(|point| pdf_point_to_screen(*point, page_rect, source_size))
        .collect::<Vec<_>>();
    if points.len() > 1 {
        painter.add(egui::Shape::line(points, Stroke::new(2.2, color)));
    }
}

fn annotation_at(
    annotations: &[Annotation],
    point: egui::Pos2,
    page_rect: Rect,
    source_size: [f32; 2],
) -> Option<u64> {
    annotations
        .iter()
        .rev()
        .find(|annotation| match &annotation.kind {
            AnnotationKind::Highlight { rects, .. } => rects.iter().any(|bounds| {
                pdf_bounds_screen_rect(*bounds, page_rect, source_size)
                    .expand(5.0)
                    .contains(point)
            }),
            AnnotationKind::Ink { points, .. } => points.iter().any(|value| {
                pdf_point_to_screen(*value, page_rect, source_size).distance(point) < 9.0
            }),
            AnnotationKind::Note { position, .. } => {
                pdf_point_to_screen(*position, page_rect, source_size).distance(point) < 14.0
            }
            AnnotationKind::TextBox { rect, .. } | AnnotationKind::Shape { rect, .. } => {
                pdf_bounds_screen_rect(*rect, page_rect, source_size)
                    .expand(5.0)
                    .contains(point)
            }
            AnnotationKind::Stamp { position, text, .. } => {
                let anchor = pdf_point_to_screen(*position, page_rect, source_size);
                Rect::from_min_size(
                    anchor,
                    vec2((text.len() as f32 * 8.0 + 16.0).max(72.0), 28.0),
                )
                .contains(point)
            }
            AnnotationKind::Attachment { position, .. } => {
                pdf_point_to_screen(*position, page_rect, source_size).distance(point) < 16.0
            }
        })
        .map(|annotation| annotation.id)
}

fn text_in_pdf_region(glyphs: &[TextGlyph], bounds: [f32; 4]) -> String {
    let [left, bottom, right, top] = bounds;
    glyphs
        .iter()
        .filter_map(|glyph| {
            let glyph_bounds = glyph.bounds?;
            let center_x = (glyph_bounds[0] + glyph_bounds[2]) * 0.5;
            let center_y = (glyph_bounds[1] + glyph_bounds[3]) * 0.5;
            (center_x >= left.min(right)
                && center_x <= left.max(right)
                && center_y >= bottom.min(top)
                && center_y <= bottom.max(top))
            .then_some(glyph.text.as_str())
        })
        .collect::<String>()
}

fn installed_printers() -> Vec<String> {
    let Ok(output) = Command::new("lpstat").arg("-a").output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

fn default_printer() -> Option<String> {
    let output = Command::new("lpstat").arg("-d").output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_once(':')
        .map(|(_, printer)| printer.trim().to_owned())
        .filter(|printer| !printer.is_empty())
}

/// Adds useful links when a PDF exposes an outline or printed URL but omitted
/// the corresponding page annotations (a common occurrence in generated TOCs).
fn infer_text_links(
    glyphs: &[TextGlyph],
    outline: &[PdfOutlineItem],
    existing: &[PdfLinkRegion],
) -> Vec<PdfLinkRegion> {
    let mut inferred = Vec::new();
    let (normalized_page, normalized_map) = normalized_glyph_stream(glyphs);

    for item in outline {
        let Some(target) = item.target.clone() else {
            continue;
        };
        let needle = normalize_link_text(&item.title);
        if needle.chars().count() < 4 {
            continue;
        }
        for (start, _) in normalized_page.match_indices(&needle) {
            let end = start + needle.len();
            let start_char = normalized_page[..start].chars().count();
            let end_char = normalized_page[..end].chars().count();
            if let Some(bounds) = union_glyph_bounds(
                glyphs,
                &normalized_map[start_char..end_char.min(normalized_map.len())],
            ) {
                push_non_overlapping_link(
                    &mut inferred,
                    existing,
                    PdfLinkRegion {
                        bounds,
                        target: target.clone(),
                    },
                );
            }
        }
    }

    let (raw_page, raw_map) = raw_glyph_stream(glyphs);
    for (byte_start, token) in raw_page.split_word_bound_indices() {
        let trimmed = token.trim_matches(|character: char| {
            character.is_whitespace()
                || matches!(
                    character,
                    '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';'
                )
        });
        let trimmed = trimmed.trim_end_matches(['.', ':', '!', '?']);
        let lower = trimmed.to_ascii_lowercase();
        if !(lower.starts_with("https://")
            || lower.starts_with("http://")
            || lower.starts_with("www."))
        {
            continue;
        }
        let leading = token.find(trimmed).unwrap_or(0);
        let start = byte_start + leading;
        let end = start + trimmed.len();
        let start_char = raw_page[..start].chars().count();
        let end_char = raw_page[..end].chars().count();
        if let Some(bounds) =
            union_glyph_bounds(glyphs, &raw_map[start_char..end_char.min(raw_map.len())])
        {
            let uri = if lower.starts_with("www.") {
                format!("https://{trimmed}")
            } else {
                trimmed.to_owned()
            };
            push_non_overlapping_link(
                &mut inferred,
                existing,
                PdfLinkRegion {
                    bounds,
                    target: LinkTarget::Uri(uri),
                },
            );
        }
    }
    inferred
}

trait WordBoundIndices {
    fn split_word_bound_indices(&self) -> Vec<(usize, &str)>;
}

impl WordBoundIndices for str {
    fn split_word_bound_indices(&self) -> Vec<(usize, &str)> {
        let mut result = Vec::new();
        let mut start = None;
        for (index, character) in self.char_indices() {
            if character.is_whitespace() {
                if let Some(begin) = start.take() {
                    result.push((begin, &self[begin..index]));
                }
            } else if start.is_none() {
                start = Some(index);
            }
        }
        if let Some(begin) = start {
            result.push((begin, &self[begin..]));
        }
        result
    }
}

fn normalized_glyph_stream(glyphs: &[TextGlyph]) -> (String, Vec<usize>) {
    let mut text = String::new();
    let mut mapping = Vec::new();
    let mut previous_was_space = true;
    for (glyph_index, glyph) in glyphs.iter().enumerate() {
        for character in glyph.text.chars().flat_map(char::to_lowercase) {
            if character.is_alphanumeric() {
                text.push(character);
                mapping.push(glyph_index);
                previous_was_space = false;
            } else if !previous_was_space {
                text.push(' ');
                mapping.push(glyph_index);
                previous_was_space = true;
            }
        }
    }
    if text.ends_with(' ') {
        text.pop();
        mapping.pop();
    }
    (text, mapping)
}

fn normalize_link_text(text: &str) -> String {
    let glyphs = text
        .chars()
        .map(|character| TextGlyph {
            text: character.to_string(),
            bounds: None,
        })
        .collect::<Vec<_>>();
    normalized_glyph_stream(&glyphs).0
}

fn raw_glyph_stream(glyphs: &[TextGlyph]) -> (String, Vec<usize>) {
    let mut text = String::new();
    let mut mapping = Vec::new();
    for (glyph_index, glyph) in glyphs.iter().enumerate() {
        for character in glyph.text.chars() {
            text.push(character);
            mapping.push(glyph_index);
        }
    }
    (text, mapping)
}

fn union_glyph_bounds(glyphs: &[TextGlyph], indices: &[usize]) -> Option<[f32; 4]> {
    let mut bounds: Option<[f32; 4]> = None;
    for index in indices.iter().copied() {
        let Some(next) = glyphs.get(index).and_then(|glyph| glyph.bounds) else {
            continue;
        };
        bounds = Some(match bounds {
            None => next,
            Some([left, bottom, right, top]) => [
                left.min(next[0]),
                bottom.min(next[1]),
                right.max(next[2]),
                top.max(next[3]),
            ],
        });
    }
    bounds
}

fn push_non_overlapping_link(
    inferred: &mut Vec<PdfLinkRegion>,
    existing: &[PdfLinkRegion],
    candidate: PdfLinkRegion,
) {
    if existing
        .iter()
        .chain(inferred.iter())
        .any(|link| pdf_bounds_overlap(link.bounds, candidate.bounds))
    {
        return;
    }
    inferred.push(candidate);
}

fn pdf_bounds_overlap(a: [f32; 4], b: [f32; 4]) -> bool {
    let horizontal = a[0].max(b[0]) <= a[2].min(b[2]);
    let vertical = a[1].max(b[1]) <= a[3].min(b[3]);
    horizontal && vertical
}

fn available_pdf_apps() -> Vec<ExternalApp> {
    let mut directories = Vec::new();
    if let Some(data) = dirs::data_local_dir() {
        directories.push(data.join("applications"));
    }
    if let Some(home) = dirs::home_dir() {
        directories.push(home.join(".local/share/flatpak/exports/share/applications"));
    }
    directories.extend([
        PathBuf::from("/usr/local/share/applications"),
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
    ]);

    let mut seen = HashSet::new();
    let mut apps = Vec::new();
    for directory in directories {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("desktop") {
                continue;
            }
            let Some(id) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if id == "io.snappdf.SnapPDF.desktop" || !seen.insert(id.to_owned()) {
                continue;
            }
            let Ok(contents) = fs::read_to_string(&path) else {
                continue;
            };
            if let Some(name) = pdf_desktop_app_name(&contents) {
                apps.push(ExternalApp {
                    name,
                    desktop_file: path,
                });
            }
        }
    }
    apps.sort_by(|left, right| left.name.to_lowercase().cmp(&right.name.to_lowercase()));
    apps
}

fn pdf_desktop_app_name(contents: &str) -> Option<String> {
    let hidden = contents
        .lines()
        .any(|line| matches!(line.trim(), "Hidden=true" | "NoDisplay=true"));
    let supports_pdf = contents.lines().any(|line| {
        line.trim()
            .strip_prefix("MimeType=")
            .is_some_and(|types| types.split(';').any(|value| value == "application/pdf"))
    });
    if hidden || !supports_pdf {
        return None;
    }
    contents
        .lines()
        .find_map(|line| line.trim().strip_prefix("Name="))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_pdf_text_is_copied_in_document_order() {
        let glyphs = "Crisp PDF"
            .chars()
            .map(|character| TextGlyph {
                text: character.to_string(),
                bounds: None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            selected_glyph_text(&glyphs, 8, 0).as_deref(),
            Some("Crisp PDF")
        );
    }

    #[test]
    fn only_browser_safe_pdf_links_are_opened() {
        assert!(is_safe_external_uri("https://example.com"));
        assert!(is_safe_external_uri("mailto:reader@example.com"));
        assert!(!is_safe_external_uri("javascript:alert(1)"));
        assert!(!is_safe_external_uri("file:///etc/passwd"));
    }

    #[test]
    fn outline_titles_become_clickable_when_pdf_annotations_are_missing() {
        let glyphs = "Contents  Energy and geodesics"
            .chars()
            .enumerate()
            .map(|(index, character)| TextGlyph {
                text: character.to_string(),
                bounds: Some([index as f32, 10.0, index as f32 + 1.0, 20.0]),
            })
            .collect::<Vec<_>>();
        let links = infer_text_links(
            &glyphs,
            &[PdfOutlineItem {
                title: "Energy and geodesics".to_owned(),
                depth: 1,
                target: Some(LinkTarget::Page(12)),
            }],
            &[],
        );
        assert_eq!(links.len(), 1);
        assert!(matches!(links[0].target, LinkTarget::Page(12)));
    }

    #[test]
    fn printed_web_addresses_become_clickable() {
        let glyphs = "See https://example.com/docs."
            .chars()
            .enumerate()
            .map(|(index, character)| TextGlyph {
                text: character.to_string(),
                bounds: Some([index as f32, 0.0, index as f32 + 1.0, 8.0]),
            })
            .collect::<Vec<_>>();
        let links = infer_text_links(&glyphs, &[], &[]);
        assert_eq!(links.len(), 1);
        assert!(
            matches!(&links[0].target, LinkTarget::Uri(uri) if uri == "https://example.com/docs")
        );
    }

    #[test]
    fn open_with_only_lists_visible_pdf_apps() {
        let desktop =
            "[Desktop Entry]\nName=Document Reader\nMimeType=image/png;application/pdf;\n";
        assert_eq!(
            pdf_desktop_app_name(desktop).as_deref(),
            Some("Document Reader")
        );
        assert!(
            pdf_desktop_app_name(
                "[Desktop Entry]\nName=Hidden\nNoDisplay=true\nMimeType=application/pdf;\n"
            )
            .is_none()
        );
        assert!(
            pdf_desktop_app_name("[Desktop Entry]\nName=Text Editor\nMimeType=text/plain;\n")
                .is_none()
        );
    }

    #[test]
    fn comments_created_from_highlights_anchor_above_the_highlight() {
        let highlight = Annotation {
            id: 7,
            page: 2,
            kind: AnnotationKind::Highlight {
                rects: vec![[10.0, 20.0, 30.0, 32.0], [12.0, 8.0, 44.0, 18.0]],
                color: [255, 220, 0, 90],
            },
        };
        assert_eq!(annotation_comment_position(&highlight), Some([49.0, 32.0]));
    }

    #[test]
    fn comment_and_highlight_hit_targets_are_independent() {
        let annotations = vec![
            Annotation {
                id: 10,
                page: 0,
                kind: AnnotationKind::Highlight {
                    rects: vec![[10.0, 10.0, 60.0, 30.0]],
                    color: [255, 220, 0, 90],
                },
            },
            Annotation {
                id: 11,
                page: 0,
                kind: AnnotationKind::Note {
                    position: [20.0, 20.0],
                    text: "Comment".to_owned(),
                    color: [255, 220, 0, 255],
                    parent: Some(10),
                },
            },
        ];
        let page = Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 100.0));
        assert_eq!(
            annotation_at(&annotations, pos2(20.0, 80.0), page, [100.0, 100.0]),
            Some(11)
        );
        assert_eq!(
            annotation_at(&annotations, pos2(50.0, 80.0), page, [100.0, 100.0]),
            Some(10)
        );
    }

    #[test]
    fn native_trackpad_pinch_changes_pdf_zoom() {
        let factor = trackpad_pinch_factor(&[egui::Event::Zoom(1.25)]);
        assert!((factor - 1.25).abs() < f32::EPSILON);
    }

    #[test]
    fn x11_precision_pinch_is_supported_but_mouse_wheel_is_not() {
        let modifiers = Modifiers {
            ctrl: true,
            ..Default::default()
        };
        let precision_pinch = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, 20.0),
            phase: egui::TouchPhase::Move,
            modifiers,
        };
        let mouse_wheel = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0.0, 3.0),
            phase: egui::TouchPhase::Move,
            modifiers,
        };

        assert!(trackpad_pinch_factor(&[precision_pinch]) > 1.0);
        assert_eq!(trackpad_pinch_factor(&[mouse_wheel]), 1.0);
    }
    #[test]
    fn pinch_preserves_document_point_using_absolute_scroll_offset() {
        let pages = [[400.0, 600.0], [400.0, 800.0]];
        let anchor = PinchAnchor {
            page: 1,
            fraction: vec2(0.25, 0.5),
        };
        let actual = pinch_anchored_offset(
            vec2(50.0, 200.0),
            anchor,
            &pages,
            1.0,
            1.5,
            600.0,
            656.0,
            false,
        );
        // Y: 200 + (600 + 800*0.5)*0.5 = 700.
        // X: 50 + (656-600)/2 + (0.25-0.5)*400*0.5 = 28.
        assert!((actual.y - 700.0).abs() < 0.001);
        assert!((actual.x - 28.0).abs() < 0.001);
    }

    #[test]
    fn pinch_offset_excludes_hidden_pages_in_presentation_mode() {
        let pages = [[400.0, 600.0], [400.0, 800.0]];
        let anchor = PinchAnchor {
            page: 1,
            fraction: vec2(0.5, 0.25),
        };
        let actual = pinch_anchored_offset(
            vec2(0.0, 100.0),
            anchor,
            &pages,
            1.0,
            1.5,
            700.0,
            700.0,
            true,
        );
        assert!((actual.y - 200.0).abs() < 0.001);
        assert_eq!(actual.x, 0.0);
    }

    #[test]
    fn pinch_offset_never_goes_negative() {
        let anchor = PinchAnchor {
            page: 0,
            fraction: vec2(0.2, 0.2),
        };
        let actual = pinch_anchored_offset(
            vec2(0.0, 0.0),
            anchor,
            &[[400.0, 600.0]],
            2.0,
            1.0,
            856.0,
            456.0,
            false,
        );
        assert_eq!(actual, egui::Vec2::ZERO);
    }

    #[test]
    fn selection_can_extend_across_pages_in_either_direction() {
        let start = TextPosition { page: 2, glyph: 3 };
        let end = TextPosition { page: 4, glyph: 1 };
        for selection in [
            TextSelection {
                anchor: start,
                focus: end,
            },
            TextSelection {
                anchor: end,
                focus: start,
            },
        ] {
            assert_eq!(selection.ordered(), (start, end));
            assert_eq!(selection.bounds_on_page(1, 9), None);
            assert_eq!(selection.bounds_on_page(2, 6), Some((3, 5)));
            assert_eq!(selection.bounds_on_page(3, 8), Some((0, 7)));
            assert_eq!(selection.bounds_on_page(4, 5), Some((0, 1)));
            assert_eq!(selection.bounds_on_page(5, 3), None);
        }
    }

    #[test]
    fn text_drag_scrolls_up_and_down_near_the_viewport_edge() {
        let viewport = Rect::from_min_max(pos2(0.0, 100.0), pos2(900.0, 700.0));
        assert!(selection_edge_scroll(101.0, viewport) > 0.0); // Content down, view up
        assert!(selection_edge_scroll(699.0, viewport) < 0.0); // Content up, view down
        assert_eq!(selection_edge_scroll(400.0, viewport), 0.0);
    }

    #[test]
    fn drag_extends_selection_in_page_margin() {
        let page = Rect::from_min_max(pos2(0.0, 0.0), pos2(200.0, 200.0));
        let glyphs = vec![
            TextGlyph {
                text: "A".to_owned(),
                bounds: Some([15.0, 175.0, 25.0, 190.0]),
            },
            TextGlyph {
                text: "B".to_owned(),
                bounds: Some([15.0, 10.0, 25.0, 25.0]),
            },
        ];
        assert_eq!(
            nearest_drag_glyph(&glyphs, page, [200.0, 200.0], pos2(150.0, 230.0)),
            Some(1)
        );
        assert_eq!(
            nearest_drag_glyph(&glyphs, page, [200.0, 200.0], pos2(150.0, -20.0)),
            Some(0)
        );
    }
}
