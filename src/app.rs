use crate::chat::{
    ChatAttachment, ChatCommand, ChatEvent, ChatMessage, ChatProvider, ChatRole, ChatWorker,
    ReasoningEffort, mime_for_path, model_supports_reasoning_effort,
};
use crate::instance::{self, LaunchTarget};
use crate::model::{
    Annotation, AnnotationKind, AppState, DEFAULT_CHROME_SCALE_RATIO, DEFAULT_SIDEBAR_SCALE_RATIO,
    DEFAULT_UI_SCALE, PathBarStyle, RecentDocument, SidebarFileIcons, StudyChatSession,
    is_discoverable_pdf, is_pdf,
};
use crate::ocr::{OcrCommand, OcrEvent, OcrWorker};
use crate::pdf::{
    LinkTarget, PREVIEW_RENDER_WIDTH, PdfFileSearchMatches, PdfLinkRegion, PdfOutlineItem,
    PdfSearchMatch, RenderCommand, RenderEvent, RenderWorker, TextGlyph, search_pdf_files,
};
use crate::update::{self, UpdateEvent, UpdateStatus, UpdateWorker};
use crate::{storage, theme};
use anyhow::{Context, Result, anyhow};
use eframe::egui::{
    self, Align, Align2, Color32, ColorImage, CornerRadius, CursorIcon, FontId, Frame, Key,
    KeyboardShortcut, Layout, Margin, Modifiers, Rect, Response, RichText, ScrollArea, Sense,
    Stroke, StrokeKind, TextureHandle, TextureOptions, pos2, vec2,
};
use latex_rust::{Color as LatexColor, Dim, MathFont, PngOptions, latex_to_png};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    OnceLock,
    mpsc::{self, Receiver, TryRecvError},
};
use std::time::{SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

const MAX_TEXTURES: usize = 10;
const PAGE_ENTRY_ID: &str = "toolbar-page-entry";
const MAX_TEXTURE_BYTES: usize = 256 * 1024 * 1024;
const MAX_PENDING_PAGES: usize = 16;
const PAGE_GAP: f32 = 18.0;
const KEYBOARD_TAP_SCROLL: f32 = 150.0;
const WHEEL_SCROLL_MULTIPLIER: f32 = 1.65;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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
    Chat,
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
    ChatCapture,
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

struct StudyChat {
    visible: bool,
    provider: ChatProvider,
    connected: HashMap<ChatProvider, bool>,
    accounts: HashMap<ChatProvider, String>,
    models: HashMap<ChatProvider, Vec<String>>,
    selected_models: HashMap<ChatProvider, String>,
    sessions: Vec<StudyChatSession>,
    active_session_id: u64,
    pending_replacement_warning: bool,
    prompt: String,
    attachments: Vec<ChatAttachment>,
    status: String,
    busy: bool,
    active_request: Option<u64>,
    active_provider: Option<ChatProvider>,
    next_request: u64,
    gemini_key: String,
    show_gemini_key: bool,
    renaming_session: Option<u64>,
    rename_text: String,
    rename_focus_requested: bool,
}

impl StudyChat {
    fn from_state(state: &AppState) -> Self {
        let mut sessions = state.study_chats.clone();
        if sessions.is_empty() {
            sessions.push(new_study_chat_session(1));
        }
        for session in &mut sessions {
            restore_legacy_chat_title(session);
        }
        sessions.truncate(5);
        let active_session_id = state
            .active_study_chat
            .filter(|id| sessions.iter().any(|session| session.id == *id))
            .unwrap_or(sessions[0].id);
        Self {
            visible: false,
            provider: ChatProvider::ChatGpt,
            connected: HashMap::new(),
            accounts: HashMap::new(),
            models: HashMap::new(),
            selected_models: HashMap::new(),
            sessions,
            active_session_id,
            pending_replacement_warning: false,
            prompt: String::new(),
            attachments: Vec::new(),
            status: "Connect a provider to start studying.".to_owned(),
            busy: false,
            active_request: None,
            active_provider: None,
            next_request: 1,
            gemini_key: String::new(),
            show_gemini_key: false,
            renaming_session: None,
            rename_text: String::new(),
            rename_focus_requested: false,
        }
    }

    fn active_session(&self) -> &StudyChatSession {
        self.sessions
            .iter()
            .find(|session| session.id == self.active_session_id)
            .unwrap_or(&self.sessions[0])
    }

    fn active_session_mut(&mut self) -> &mut StudyChatSession {
        let index = self
            .sessions
            .iter()
            .position(|session| session.id == self.active_session_id)
            .unwrap_or(0);
        &mut self.sessions[index]
    }
}

fn new_study_chat_session(id: u64) -> StudyChatSession {
    StudyChatSession {
        id,
        title: "New chat".to_owned(),
        messages: HashMap::new(),
        last_opened: unix_timestamp(),
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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

struct PasswordPrompt {
    path: PathBuf,
    input: String,
    /// The previous attempt was rejected.
    incorrect: bool,
    focus_requested: bool,
}

/// Queue `path` with CUPS and describe the result. `lp` only copies the job
/// into the spooler, so waiting for it is quick and lets real errors (no
/// printer, bad page range) reach the user instead of a false "submitted".
fn send_to_printer(path: &Path, dialog: &PrintDialog) -> String {
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
    command.arg(path);
    match command.output() {
        Ok(output) if output.status.success() => {
            let detail = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if detail.is_empty() {
                "Sent to the printer.".to_owned()
            } else {
                format!("Sent to the printer ({detail}).")
            }
        }
        Ok(output) => format!(
            "Printing failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(error) => format!("Could not start printing (is CUPS installed?): {error}"),
    }
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
    Rename(PathBuf, String, Rect),
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

fn launch_targets_from_args(
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> Vec<LaunchTarget> {
    args.into_iter()
        .filter_map(|argument| {
            let path = PathBuf::from(argument);
            let path = path.canonicalize().unwrap_or(path);
            if path.is_dir() {
                Some(LaunchTarget::Project(path))
            } else if path.is_file() && is_pdf(&path) {
                Some(LaunchTarget::Pdf(path))
            } else {
                None
            }
        })
        .collect()
}

pub fn run() -> Result<()> {
    let (flags, args): (Vec<_>, Vec<_>) = std::env::args_os()
        .skip(1)
        .partition(|argument| argument == "--new-window");
    let launch_targets = launch_targets_from_args(args);
    let socket = instance::socket_path();
    let listener = if flags.is_empty() {
        match instance::claim(&socket, &launch_targets) {
            instance::Claim::Forwarded => return Ok(()),
            instance::Claim::Primary(listener) => listener,
        }
    } else {
        None
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("SnaPdf")
            .with_app_id("io.snapdf.SnaPdf")
            .with_inner_size([1360.0, 860.0])
            .with_min_inner_size([820.0, 560.0]),
        #[cfg(target_os = "linux")]
        event_loop_builder: linux_event_loop_builder(),
        ..Default::default()
    };

    eframe::run_native(
        "SnaPdf",
        options,
        Box::new(move |creation| {
            let instance = listener.map(|listener| {
                (
                    instance::listen(listener, creation.egui_ctx.clone()),
                    socket,
                )
            });
            Ok(Box::new(SnaPdfApp::new(creation, launch_targets, instance)))
        }),
    )
    .map_err(|error| anyhow!(error.to_string()))
}

/// Keep the application typography unchanged while providing portable glyph
/// fallbacks for model output. egui does not consult system fontconfig at
/// runtime, so without these bundled faces an AppImage can display tofu boxes
/// for perfectly valid arrows, technical symbols, and scientific notation.
fn install_unicode_fallback_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for (name, bytes) in [
        (
            "SnaPdf-NotoSans",
            include_bytes!("../resources/fonts/NotoSans-Regular.ttf").as_slice(),
        ),
        (
            "SnaPdf-NotoSymbols",
            include_bytes!("../resources/fonts/NotoSansSymbols-Regular.ttf").as_slice(),
        ),
        (
            "SnaPdf-NotoSymbols2",
            include_bytes!("../resources/fonts/NotoSansSymbols2-Regular.ttf").as_slice(),
        ),
    ] {
        fonts.font_data.insert(
            name.to_owned(),
            std::sync::Arc::new(egui::FontData::from_static(bytes)),
        );
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts
                .families
                .get_mut(&family)
                .expect("egui default font family")
                .push(name.to_owned());
        }
    }
    ctx.set_fonts(fonts);
}

/// Winit does not currently expose native touchpad pinch events on Linux
/// Wayland. When XWayland is available, run SnaPdf's window through X11 so the
/// XI 2.4 listener can receive real begin/update/end pinch gestures. Respect an
/// explicit winit backend choice and keep native Wayland on systems without an
/// X display.
#[cfg(target_os = "linux")]
fn linux_event_loop_builder() -> Option<eframe::EventLoopBuilderHook> {
    if !should_prefer_x11_gestures(
        std::env::var_os("XDG_SESSION_TYPE").as_deref(),
        std::env::var_os("WAYLAND_DISPLAY").as_deref(),
        std::env::var_os("DISPLAY").as_deref(),
        std::env::var_os("WINIT_UNIX_BACKEND").as_deref(),
    ) {
        return None;
    }

    Some(Box::new(|builder| {
        use winit::platform::x11::EventLoopBuilderExtX11 as _;
        builder.with_x11();
    }))
}

#[cfg(target_os = "linux")]
fn should_prefer_x11_gestures(
    session_type: Option<&std::ffi::OsStr>,
    wayland_display: Option<&std::ffi::OsStr>,
    x_display: Option<&std::ffi::OsStr>,
    explicit_backend: Option<&std::ffi::OsStr>,
) -> bool {
    if explicit_backend.is_some() || x_display.is_none() {
        return false;
    }

    session_type.is_some_and(|value| value.eq_ignore_ascii_case("wayland"))
        || wayland_display.is_some()
}

pub struct SnaPdfApp {
    state: AppState,
    /// Targets forwarded by later launches while this window owns the socket.
    instance_requests: Option<crossbeam_channel::Receiver<Vec<LaunchTarget>>>,
    instance_socket: Option<PathBuf>,
    update_worker: UpdateWorker,
    update_status: UpdateStatus,
    /// A newer release was found this session and the prompt is still open.
    show_update_prompt: bool,
    /// Frozen at startup so clicking a PDF never reshuffles the visible recent list.
    session_recents: Vec<RecentDocument>,
    worker: RenderWorker,
    ocr_worker: OcrWorker,
    chat_worker: ChatWorker,
    study_chat: StudyChat,
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
    pending_select_all_text: bool,
    pending_selection_copy: bool,
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
    rename_anchor: Option<Rect>,
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
    chat_capture_requests: HashMap<u64, usize>,
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
    /// Focus the search field (selecting any previous query) on the next frame.
    search_focus_requested: bool,
    print_dialog: Option<PrintDialog>,
    browser_directory: Option<PathBuf>,
    browser_entries: Vec<BrowserEntry>,
    browser_scan: Option<Receiver<BrowserScanResult>>,
    pinned_comment: Option<u64>,
    show_view_settings: bool,
    settings_page: SettingsPage,
    /// Passwords for encrypted PDFs, kept only for this session.
    document_passwords: HashMap<PathBuf, String>,
    /// An annotated copy being prepared for printing, and the print settings.
    pending_print: Option<(PathBuf, PrintDialog)>,
    password_prompt: Option<PasswordPrompt>,
    pending_interface_scale: Option<f32>,
    pdf_default_application: Option<String>,
    confirm_save_to_source: bool,
    pending_source_save: Option<(PathBuf, PathBuf)>,
}

impl SnaPdfApp {
    fn new(
        creation: &eframe::CreationContext<'_>,
        launch_targets: Vec<LaunchTarget>,
        instance: Option<(crossbeam_channel::Receiver<Vec<LaunchTarget>>, PathBuf)>,
    ) -> Self {
        install_unicode_fallback_fonts(&creation.egui_ctx);
        theme::apply(&creation.egui_ctx);
        let mut state = storage::load();
        state.prune_missing();
        state.normalize_recents();
        let mut launch_pdfs = Vec::new();
        for target in launch_targets {
            match target {
                LaunchTarget::Pdf(path) => launch_pdfs.push(path),
                LaunchTarget::Project(root) => {
                    if state.add_project(root) {
                        let _ = storage::save(&state);
                    }
                }
            }
        }
        let initial_path = launch_pdfs.pop();
        if state.study_chats.is_empty() && state.active_study_chat.is_none() {
            state.warn_before_replacing_study_chat = true;
        }
        let initial_path = initial_path
            .or_else(|| state.active_document.clone())
            .or_else(|| state.last_document.clone());
        if let Some(path) = initial_path.as_ref() {
            state.record_open(path);
        }
        creation
            .egui_ctx
            .set_zoom_factor(interface_zoom(state.ui_scale));
        let session_recents = state.recent_documents.clone();
        let project_files = state
            .projects
            .iter()
            .map(|project| (project.root.clone(), project.pdf_files()))
            .collect();

        let ocr_worker = OcrWorker::spawn(creation.egui_ctx.clone());
        let _ = ocr_worker.commands.send(OcrCommand::Warmup);
        let study_chat = StudyChat::from_state(&state);
        let mut app = Self {
            state,
            session_recents,
            worker: RenderWorker::spawn(creation.egui_ctx.clone()),
            ocr_worker,
            chat_worker: ChatWorker::spawn(creation.egui_ctx.clone()),
            study_chat,
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
            pending_select_all_text: false,
            pending_selection_copy: false,
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
            rename_anchor: None,
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
            chat_capture_requests: HashMap::new(),
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
            search_focus_requested: false,
            print_dialog: None,
            browser_directory: None,
            browser_entries: Vec::new(),
            browser_scan: None,
            pinned_comment: None,
            show_view_settings: false,
            settings_page: SettingsPage::General,
            document_passwords: HashMap::new(),
            pending_print: None,
            password_prompt: None,
            pending_interface_scale: None,
            pdf_default_application: None,
            confirm_save_to_source: false,
            pending_source_save: None,
            instance_requests: instance.as_ref().map(|(receiver, _)| receiver.clone()),
            instance_socket: instance.map(|(_, socket)| socket),
            update_worker: UpdateWorker::spawn(creation.egui_ctx.clone()),
            update_status: UpdateStatus::Checking,
            show_update_prompt: false,
        };

        app.update_worker.check();
        // Earlier command-line PDFs become background tabs; the last one is shown.
        for path in launch_pdfs {
            if path.is_file() && is_pdf(&path) {
                app.state.record_open(&path);
            }
        }
        if let Some(path) = initial_path {
            app.open_document(path);
        }
        app
    }

    fn receive_update_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.update_worker.events.try_recv() {
            match event {
                UpdateEvent::Checked(Ok(Some(release))) => {
                    self.show_update_prompt = true;
                    self.update_status = UpdateStatus::Available(release);
                }
                UpdateEvent::Checked(Ok(None)) => self.update_status = UpdateStatus::UpToDate,
                UpdateEvent::Checked(Err(error)) => {
                    self.update_status = UpdateStatus::Failed(error);
                }
                UpdateEvent::Installed(Ok(())) => self.restart_after_update(ctx),
                // A cancelled download already put the status back to Available.
                UpdateEvent::Installed(Err(error)) if error == update::CANCELLED => {}
                UpdateEvent::Installed(Err(error)) => {
                    self.update_status = UpdateStatus::Failed(error);
                }
            }
        }
    }

    fn start_update(&mut self) {
        let UpdateStatus::Available(release) = self.update_status.clone() else {
            return;
        };
        let Some(target) = update::self_update_target() else {
            return;
        };
        self.update_status = UpdateStatus::Installing(release.clone());
        self.update_worker.install(release, target);
    }

    /// Start the freshly installed AppImage and close this window. The socket
    /// is released first so the new process becomes the primary window
    /// instead of handing its launch straight back to this one.
    fn restart_after_update(&mut self, ctx: &egui::Context) {
        self.persist();
        if let Some(socket) = self.instance_socket.take() {
            let _ = fs::remove_file(socket);
        }
        match update::relaunch_command().map(|command| Command::new(command).spawn()) {
            Some(Ok(_)) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            _ => {
                self.update_status = UpdateStatus::Failed(
                    "the update is installed; restart SnaPdf to use it".to_owned(),
                );
            }
        }
    }

    fn settings_updates_card(&mut self, ui: &mut egui::Ui) {
        let can_self_update = update::self_update_target().is_some();
        let busy = matches!(
            self.update_status,
            UpdateStatus::Checking | UpdateStatus::Installing(_)
        );
        let (status, color) = match &self.update_status {
            UpdateStatus::Checking => ("Checking for updates…".to_owned(), theme::MUTED),
            UpdateStatus::UpToDate => ("You're on the latest release.".to_owned(), SETTINGS_OK),
            UpdateStatus::Available(release) => (
                format!("Version {} is available.", release.version),
                Color32::from_rgb(255, 196, 92),
            ),
            UpdateStatus::Installing(release) => {
                let (done, total) = self.update_worker.progress.bytes();
                let megabytes = |bytes: u64| bytes as f64 / 1_048_576.0;
                let detail = if total > 0 {
                    format!(
                        "{:.1} of {:.1} MB ({}%)",
                        megabytes(done),
                        megabytes(total),
                        done * 100 / total
                    )
                } else {
                    format!("{:.1} MB", megabytes(done))
                };
                (
                    format!("Downloading version {}: {detail}", release.version),
                    theme::MUTED,
                )
            }
            UpdateStatus::Failed(error) => (
                format!("Couldn't update: {error}"),
                Color32::from_rgb(235, 120, 110),
            ),
        };
        let mut start_update = false;
        let mut check = false;
        let mut cancel = false;
        settings_card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(format!("SnaPdf {}", update::CURRENT_VERSION))
                            .size(15.0)
                            .strong()
                            .color(Color32::from_rgb(236, 236, 236)),
                    );
                    ui.horizontal(|ui| {
                        if busy {
                            ui.spinner();
                        }
                        ui.label(RichText::new(&status).size(12.5).color(color));
                    });
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    match &self.update_status {
                        UpdateStatus::Available(release) if !can_self_update => {
                            if ui.button("Release page").clicked() {
                                let _ = webbrowser::open(&release.page_url);
                            }
                        }
                        UpdateStatus::Available(_) => {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("Update and restart").color(Color32::BLACK),
                                    )
                                    .fill(SETTINGS_ACCENT),
                                )
                                .clicked()
                            {
                                start_update = true;
                            }
                        }
                        UpdateStatus::Installing(_) => {
                            if ui.button("Cancel").clicked() {
                                cancel = true;
                            }
                        }
                        _ if !busy => {
                            if ui.button("Check for updates").clicked() {
                                check = true;
                            }
                        }
                        _ => {}
                    }
                });
            });
            if !can_self_update && matches!(self.update_status, UpdateStatus::Available(_)) {
                ui.label(
                    RichText::new(
                        "This copy wasn't installed as the portable AppImage, so update it the way you installed it.",
                    )
                    .size(11.5)
                    .color(theme::MUTED),
                );
            }
        });
        if start_update {
            self.start_update();
        }
        if cancel && let UpdateStatus::Installing(release) = self.update_status.clone() {
            self.update_worker.progress.cancel();
            self.update_status = UpdateStatus::Available(release);
        }
        if check {
            self.update_status = UpdateStatus::Checking;
            self.update_worker.check();
        }
    }

    fn password_dialog(&mut self, ctx: &egui::Context) {
        let Some(prompt) = self.password_prompt.as_mut() else {
            return;
        };
        let mut submit = false;
        let mut cancel = false;
        let path = prompt.path.clone();
        cancel |= modal_dialog(
            ctx,
            "pdf-password",
            "Password required",
            380.0,
            false,
            |ui| {
                dialog_text(
                    ui,
                    format!(
                        "{} is protected. Enter its password to open it.",
                        file_name(&path)
                    ),
                );
                let response = ui.add_sized(
                    [ui.available_width(), 32.0],
                    egui::TextEdit::singleline(&mut prompt.input)
                        .password(true)
                        .hint_text("Password")
                        .vertical_align(Align::Center)
                        .margin(Margin::symmetric(10, 6)),
                );
                if std::mem::take(&mut prompt.focus_requested) {
                    response.request_focus();
                }
                if prompt.incorrect {
                    ui.label(
                        RichText::new("That password didn't work. Try again.")
                            .size(12.5)
                            .color(Color32::from_rgb(235, 120, 110)),
                    );
                }
                submit = response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
                dialog_actions(ui, |ui| {
                    submit |= primary_button(ui, "Open", !prompt.input.is_empty()).clicked();
                    cancel |= secondary_button(ui, "Cancel").clicked();
                });
            },
        );
        if submit && !prompt.input.is_empty() {
            let prompt = self.password_prompt.take().expect("prompt is open");
            self.document_passwords
                .insert(prompt.path.clone(), prompt.input);
            self.open_document(prompt.path);
        } else if cancel {
            let prompt = self.password_prompt.take().expect("prompt is open");
            self.document_passwords.remove(&prompt.path);
            self.close_document_tab(&prompt.path);
        }
    }

    fn update_prompt_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_update_prompt || self.show_view_settings {
            return;
        }
        let UpdateStatus::Available(release) = &self.update_status else {
            self.show_update_prompt = false;
            return;
        };
        let version = release.version.clone();
        let mut open_settings = false;
        let mut later = false;
        // A corner notice rather than a modal: an update should never block reading.
        egui::Window::new("Update available")
            .id(egui::Id::new("update-prompt"))
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Foreground)
            .anchor(Align2::RIGHT_TOP, vec2(-16.0, 110.0))
            .frame(dialog_frame())
            .show(ctx, |ui| {
                ui.set_width(300.0);
                later |= dialog_header(ui, "Update available");
                dialog_text(
                    ui,
                    format!(
                        "SnaPdf {version} is out. You're on {}.",
                        update::CURRENT_VERSION
                    ),
                );
                dialog_actions(ui, |ui| {
                    open_settings = primary_button(ui, "Update", true).clicked();
                    later |= secondary_button(ui, "Later").clicked();
                });
            });
        if open_settings {
            self.show_update_prompt = false;
            self.pdf_default_application = current_pdf_default_application();
            self.settings_page = SettingsPage::General;
            self.show_view_settings = true;
        } else if later {
            self.show_update_prompt = false;
        }
    }

    fn receive_instance_handoffs(&mut self, ctx: &egui::Context) {
        let Some(receiver) = self.instance_requests.as_ref() else {
            return;
        };
        let batches: Vec<_> = receiver.try_iter().collect();
        for targets in batches {
            for target in targets {
                match target {
                    LaunchTarget::Pdf(path) => self.open_document(path),
                    LaunchTarget::Project(root) => {
                        if self.state.add_project(root.clone()) {
                            let files = self
                                .state
                                .projects
                                .iter()
                                .find(|project| project.root == root)
                                .map(|project| project.pdf_files())
                                .unwrap_or_default();
                            self.project_files.insert(root, files);
                            self.persist();
                        }
                    }
                }
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    fn open_document(&mut self, path: PathBuf) {
        if !path.is_file() || !is_pdf(&path) {
            self.status = "SnaPdf can only open PDF files.".to_owned();
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
        self.pending_select_all_text = false;
        self.pending_selection_copy = false;
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
        let password = self.document_passwords.get(&path).cloned();
        let _ = self.worker.commands.send(RenderCommand::Open {
            path,
            generation: self.generation,
            password,
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
        if let Some(paths) = rfd::FileDialog::new()
            .add_filter("PDF document", &["pdf", "PDF"])
            .pick_files()
        {
            for path in paths {
                self.open_document(path);
            }
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
        let (Some(path), Some(dialog)) = (self.current_path.clone(), self.print_dialog.take())
        else {
            return;
        };
        let annotations = self.current_annotations().to_vec();
        if annotations.is_empty() {
            self.status = send_to_printer(&path, &dialog);
            return;
        }
        // Print what the reader sees: render the annotations into a temporary
        // copy first, then hand that copy to CUPS.
        let temporary = std::env::temp_dir().join(format!(
            "snapdf-print-{}-{}.pdf",
            std::process::id(),
            unix_timestamp()
        ));
        self.status = "Preparing the annotated copy for printing...".to_owned();
        let password = self.document_passwords.get(&path).cloned();
        let _ = self.worker.commands.send(RenderCommand::ExportAnnotated {
            source: path,
            password,
            destination: temporary.clone(),
            annotations,
            generation: self.generation,
        });
        self.pending_print = Some((temporary, dialog));
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
        let password = self.document_passwords.get(&source).cloned();
        let _ = self.worker.commands.send(RenderCommand::ExportAnnotated {
            source,
            password,
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
        if self.document_passwords.contains_key(&source) {
            // Rewriting the file could drop its encryption; keep it intact.
            self.status =
                "Saving into a password-protected PDF isn't supported. Export an annotated copy instead."
                    .to_owned();
            return;
        }
        let temporary =
            source.with_file_name(format!(".{name}.snapdf-saving-{}.pdf", std::process::id()));
        let annotations = self.current_annotations().to_vec();
        self.pending_source_save = Some((temporary.clone(), source.clone()));
        self.status = "Writing annotations into the PDF...".to_owned();
        let _ = self.worker.commands.send(RenderCommand::ExportAnnotated {
            source,
            password: None,
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
                .filter(|entry| entry.file_type().is_file() && is_discoverable_pdf(entry.path()))
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
        self.state.study_chats = self.study_chat.sessions.clone();
        self.state.active_study_chat = Some(self.study_chat.active_session_id);
        if let Err(error) = storage::save(&self.state) {
            self.status = format!("Could not save app state: {error}");
        }
    }

    fn activate_study_chat(&mut self, id: u64) {
        if self.study_chat.busy || !self.study_chat.sessions.iter().any(|chat| chat.id == id) {
            return;
        }
        self.cancel_study_chat_rename();
        self.study_chat.active_session_id = id;
        self.study_chat.active_session_mut().last_opened = unix_timestamp();
        self.study_chat.attachments.clear();
        self.study_chat.prompt.clear();
        self.study_chat.status = "Ready".to_owned();
        self.persist();
    }

    fn begin_study_chat_rename(&mut self) {
        if self.study_chat.busy {
            return;
        }
        let id = self.study_chat.active_session().id;
        let title = self.study_chat.active_session().title.clone();
        self.study_chat.renaming_session = Some(id);
        self.study_chat.rename_text = title;
        self.study_chat.rename_focus_requested = true;
    }

    fn commit_study_chat_rename(&mut self) {
        let Some(id) = self.study_chat.renaming_session else {
            return;
        };
        let Some(title) = normalized_chat_title(&self.study_chat.rename_text) else {
            return;
        };
        if let Some(session) = self
            .study_chat
            .sessions
            .iter_mut()
            .find(|session| session.id == id)
        {
            session.title = title;
        }
        self.cancel_study_chat_rename();
        self.persist();
    }

    fn cancel_study_chat_rename(&mut self) {
        self.study_chat.renaming_session = None;
        self.study_chat.rename_text.clear();
        self.study_chat.rename_focus_requested = false;
    }

    fn replace_oldest_study_chat(&mut self) {
        if self.study_chat.busy {
            return;
        }
        self.cancel_study_chat_rename();
        let id = self
            .study_chat
            .sessions
            .iter()
            .map(|chat| chat.id)
            .max()
            .unwrap_or(0)
            .wrapping_add(1);
        if self.study_chat.sessions.len() < 5 {
            self.study_chat.sessions.push(new_study_chat_session(id));
        } else if let Some((index, _)) = self
            .study_chat
            .sessions
            .iter()
            .enumerate()
            .min_by_key(|(_, chat)| chat.last_opened)
        {
            self.study_chat.sessions[index] = new_study_chat_session(id);
        }
        self.study_chat.active_session_id = id;
        self.study_chat.pending_replacement_warning = false;
        self.study_chat.attachments.clear();
        self.study_chat.prompt.clear();
        self.study_chat.status = "New chat ready.".to_owned();
        self.persist();
    }

    fn request_new_study_chat(&mut self) {
        if self.study_chat.busy {
            return;
        }
        if self.study_chat.sessions.len() >= 5 && self.state.warn_before_replacing_study_chat {
            self.study_chat.pending_replacement_warning = true;
        } else {
            self.replace_oldest_study_chat();
        }
    }

    fn delete_active_study_chat(&mut self) {
        if self.study_chat.busy {
            return;
        }
        self.cancel_study_chat_rename();
        let active = self.study_chat.active_session_id;
        self.study_chat.sessions.retain(|chat| chat.id != active);
        if self.study_chat.sessions.is_empty() {
            self.study_chat
                .sessions
                .push(new_study_chat_session(active.wrapping_add(1)));
        }
        let next = self
            .study_chat
            .sessions
            .iter()
            .max_by_key(|chat| chat.last_opened)
            .map(|chat| chat.id)
            .unwrap_or(1);
        self.study_chat.active_session_id = next;
        self.study_chat.attachments.clear();
        self.study_chat.prompt.clear();
        self.study_chat.status = "Chat deleted.".to_owned();
        self.persist();
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
                RenderEvent::RenderSkipped {
                    page_index,
                    generation,
                    target_width,
                } => {
                    self.pending_pages.remove(&PageKey {
                        index: page_index,
                        width: target_width,
                        generation,
                    });
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
                    self.finish_select_all_text();
                    self.finish_pending_selection_copy(ctx);
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
                    if let Some((temporary, dialog)) = self
                        .pending_print
                        .take_if(|(temporary, _)| *temporary == destination)
                    {
                        self.status = send_to_printer(&temporary, &dialog);
                        let _ = fs::remove_file(temporary);
                        continue;
                    }
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
                        let backup_directory = source
                            .parent()
                            .unwrap_or_else(|| Path::new("."))
                            .join(".snapdf-backups");
                        let backup =
                            backup_directory.join(format!("{stem}.snapdf-backup-{timestamp}.pdf"));
                        match fs::create_dir_all(&backup_directory)
                            .and_then(|_| fs::copy(&source, &backup))
                            .and_then(|_| fs::rename(&temporary, &source))
                        {
                            Ok(_) => {
                                self.state.annotations.remove(&source);
                                self.persist();
                                self.status = format!(
                                    "Saved {exported} annotations into {}. Recovery backup: {}",
                                    file_name(&source),
                                    backup.display(),
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
                                "Exported {exported} annotations to {}; {skipped} unsupported items remain SnaPdf overlays.",
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
                    if let Some(page_index) = self.chat_capture_requests.remove(&request_id) {
                        if self.state.hide_study_chat {
                            self.tool_mode = ToolMode::Select;
                            continue;
                        }
                        match png_from_rgb(width, height, rgb) {
                            Ok(bytes) => {
                                self.study_chat.attachments.push(ChatAttachment {
                                    name: format!("PDF snip — page {}.png", page_index + 1),
                                    mime: "image/png".to_owned(),
                                    bytes,
                                });
                                self.study_chat.visible = true;
                                self.study_chat.status =
                                    "PDF image attached. Add your question and send.".to_owned();
                                self.tool_mode = ToolMode::Select;
                            }
                            Err(error) => {
                                self.study_chat.status =
                                    format!("Could not attach the PDF image: {error}");
                            }
                        }
                        continue;
                    }
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
                RenderEvent::PasswordRequired {
                    path,
                    generation,
                    incorrect,
                } if generation == self.generation => {
                    self.status = format!("{} is password-protected.", file_name(&path));
                    self.password_prompt = Some(PasswordPrompt {
                        path,
                        input: String::new(),
                        incorrect,
                        focus_requested: true,
                    });
                }
                RenderEvent::Error {
                    generation,
                    message,
                } if generation == 0 || generation == self.generation => {
                    if let Some((temporary, _)) = self.pending_source_save.take() {
                        let _ = fs::remove_file(temporary);
                    }
                    if let Some((temporary, _)) = self.pending_print.take() {
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
        while let Ok(event) = self.chat_worker.events.try_recv() {
            match event {
                ChatEvent::AccountState {
                    provider,
                    connected,
                    account,
                } => {
                    self.study_chat.connected.insert(provider, connected);
                    if let Some(account) = account {
                        self.study_chat.accounts.insert(provider, account);
                    } else {
                        self.study_chat.accounts.remove(&provider);
                    }
                    if connected {
                        self.study_chat.status = format!("{} connected.", provider.label());
                        let _ = self
                            .chat_worker
                            .commands
                            .send(ChatCommand::RefreshModels(provider));
                    }
                }
                ChatEvent::AuthProgress { provider, message } => {
                    self.study_chat.provider = provider;
                    self.study_chat.status = message;
                }
                ChatEvent::Models { provider, models } => {
                    if let Some(first) = models.first()
                        && !self.study_chat.selected_models.contains_key(&provider)
                    {
                        self.study_chat
                            .selected_models
                            .insert(provider, first.clone());
                    }
                    self.study_chat.models.insert(provider, models);
                    self.study_chat.status = format!("{} is ready.", provider.label());
                }
                ChatEvent::ResponseDelta { request_id, text }
                    if self.study_chat.active_request == Some(request_id) =>
                {
                    let provider = self.study_chat.active_provider;
                    if let Some(message) = provider
                        .and_then(|provider| {
                            self.study_chat
                                .active_session_mut()
                                .messages
                                .get_mut(&provider)
                        })
                        .and_then(|messages| messages.last_mut())
                        && message.role == ChatRole::Assistant
                    {
                        message.text.push_str(&text);
                    }
                }
                ChatEvent::ResponseComplete { request_id }
                    if self.study_chat.active_request == Some(request_id) =>
                {
                    self.study_chat.busy = false;
                    self.study_chat.active_request = None;
                    self.study_chat.active_provider = None;
                    self.study_chat.status = "Ready".to_owned();
                    self.persist();
                }
                ChatEvent::Error {
                    provider,
                    request_id,
                    message,
                } => {
                    if request_id.is_some() && request_id == self.study_chat.active_request {
                        self.study_chat.busy = false;
                        self.study_chat.active_request = None;
                        let active_provider = self.study_chat.active_provider;
                        if let Some(reply) = active_provider
                            .and_then(|provider| {
                                self.study_chat
                                    .active_session_mut()
                                    .messages
                                    .get_mut(&provider)
                            })
                            .and_then(|messages| messages.last_mut())
                            && reply.role == ChatRole::Assistant
                            && reply.text.is_empty()
                        {
                            reply.text = format!("Could not get a response: {message}");
                        }
                        self.study_chat.active_provider = None;
                    }
                    if let Some(provider) = provider {
                        self.study_chat.provider = provider;
                    }
                    self.study_chat.status = message;
                }
                _ => {}
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
            .filter(|entry| entry.file_type().is_file() && is_discoverable_pdf(entry.path()))
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
                ctx.set_zoom_factor(interface_zoom(scale));
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
                ctx.set_zoom_factor(interface_zoom(scale));
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
        // On most layouts "+" needs Shift, so Ctrl+= is the key people press.
        let zoom_in_equals = KeyboardShortcut::new(Modifiers::CTRL, Key::Equals);
        let zoom_out = KeyboardShortcut::new(Modifiers::CTRL, Key::Minus);
        let fit_width = KeyboardShortcut::new(Modifiers::CTRL, Key::Num0);
        let go_to_page = KeyboardShortcut::new(Modifiers::CTRL, Key::G);
        let copy = KeyboardShortcut::new(Modifiers::CTRL, Key::C);
        let select_all = KeyboardShortcut::new(Modifiers::CTRL, Key::A);
        let find = KeyboardShortcut::new(Modifiers::CTRL, Key::F);
        let print = KeyboardShortcut::new(Modifiers::CTRL, Key::P);
        let close_tab = KeyboardShortcut::new(Modifiers::CTRL, Key::W);
        let reopen_tab = KeyboardShortcut::new(Modifiers::CTRL | Modifiers::SHIFT, Key::T);
        let next_tab = KeyboardShortcut::new(Modifiers::CTRL, Key::Tab);
        let previous_tab = KeyboardShortcut::new(Modifiers::CTRL | Modifiers::SHIFT, Key::Tab);
        let undo = KeyboardShortcut::new(Modifiers::CTRL, Key::Z);
        let redo = KeyboardShortcut::new(Modifiers::CTRL | Modifiers::SHIFT, Key::Z);
        let toggle_study_chat = KeyboardShortcut::new(Modifiers::CTRL | Modifiers::SHIFT, Key::A);

        if self.presentation_mode {
            let (exit, previous, next, zoom_in_pressed, zoom_out_pressed) =
                ctx.input_mut(|input| {
                    (
                        input.consume_key(Modifiers::NONE, Key::Escape),
                        input.consume_key(Modifiers::NONE, Key::Backspace)
                            || input.consume_key(Modifiers::NONE, Key::ArrowLeft)
                            || input.consume_key(Modifiers::NONE, Key::PageUp)
                            || input.consume_key(Modifiers::NONE, Key::K),
                        // Presentation clickers send Page Down / Page Up.
                        input.consume_key(Modifiers::NONE, Key::Space)
                            || input.consume_key(Modifiers::NONE, Key::ArrowRight)
                            || input.consume_key(Modifiers::NONE, Key::PageDown)
                            || input.consume_key(Modifiers::NONE, Key::J),
                        input.consume_shortcut(&zoom_in) || input.consume_shortcut(&zoom_in_equals),
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

        if self.tool_mode == ToolMode::ChatCapture
            && ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape))
        {
            self.tool_mode = ToolMode::Select;
            self.drag_start = None;
            self.study_chat.status = "PDF capture cancelled.".to_owned();
            return;
        }

        if ctx.input_mut(|input| input.consume_shortcut(&toggle_study_chat))
            && !self.state.hide_study_chat
        {
            self.study_chat.visible = !self.study_chat.visible;
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
        if ctx.input_mut(|input| {
            input.consume_shortcut(&zoom_in) || input.consume_shortcut(&zoom_in_equals)
        }) {
            let cursor = ctx.input(|input| input.pointer.hover_pos());
            self.apply_pinch_zoom(1.1, cursor, ctx);
        }
        if ctx.input_mut(|input| input.consume_shortcut(&fit_width)) {
            self.fit_width_requested = true;
        }
        if !self.page_sizes.is_empty() && ctx.input_mut(|input| input.consume_shortcut(&go_to_page))
        {
            // Focus the page box with its number selected, ready to type over.
            let id = egui::Id::new(PAGE_ENTRY_ID);
            let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(self.page_entry.chars().count()),
                )));
            state.store(ctx, id);
            ctx.memory_mut(|memory| memory.request_focus(id));
        }
        // Page Up/Down and Home/End move by page unless a text field has focus.
        if !self.page_sizes.is_empty() && !ctx.egui_wants_keyboard_input() {
            let last = self.page_sizes.len() - 1;
            let target = ctx.input_mut(|input| {
                if input.consume_key(Modifiers::NONE, Key::PageDown) {
                    Some((self.current_page + 1).min(last))
                } else if input.consume_key(Modifiers::NONE, Key::PageUp) {
                    Some(self.current_page.saturating_sub(1))
                } else if input.consume_key(Modifiers::NONE, Key::Home)
                    || input.consume_key(Modifiers::CTRL, Key::Home)
                {
                    Some(0)
                } else if input.consume_key(Modifiers::NONE, Key::End)
                    || input.consume_key(Modifiers::CTRL, Key::End)
                {
                    Some(last)
                } else {
                    None
                }
            });
            if let Some(page) = target {
                self.jump_to_page = Some(page);
            }
        }
        if ctx.input_mut(|input| input.consume_shortcut(&zoom_out)) {
            let cursor = ctx.input(|input| input.pointer.hover_pos());
            self.apply_pinch_zoom(1.0 / 1.1, cursor, ctx);
        }
        let selectable_label_has_selection = ctx
            .plugin::<egui::text_selection::LabelSelectionState>()
            .lock()
            .has_selection();
        let copy_event = ctx.input(|input| {
            input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::Copy))
        });
        if !ctx.egui_wants_keyboard_input()
            && !selectable_label_has_selection
            && self.text_selection.is_some()
            && (copy_event || ctx.input_mut(|input| input.consume_shortcut(&copy)))
        {
            self.copy_text_selection(ctx);
        }
        if !ctx.egui_wants_keyboard_input()
            && self.current_path.is_some()
            && ctx.input_mut(|input| input.consume_shortcut(&select_all))
        {
            self.select_all_document_text();
        }
        if ctx.input_mut(|input| input.consume_shortcut(&find)) {
            self.open_search();
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

    fn sidebar(&mut self, root: &mut egui::Ui) {
        if !self.sidebar_visible || self.reader_mode || self.presentation_mode {
            return;
        }
        egui::Panel::left("project-sidebar")
            .resizable(true)
            .default_size(340.0)
            .min_size(0.0)
            .max_size(f32::INFINITY)
            .frame(
                Frame::new()
                    .fill(theme::SIDEBAR_BG)
                    .stroke(Stroke::new(1.0, Color32::from_rgb(48, 48, 48)))
                    .inner_margin(Margin::same(10)),
            )
            .show(root, |ui| {
                bounded_panel_content(ui, "project-sidebar-content", |ui| {
                    // The global factor is only the overall interface scale. Explorer and
                    // top-bar preferences are independent local ratios.
                    let explorer_factor = interface_region_scale(self.state.sidebar_scale_ratio);
                    ui.style_mut().spacing.item_spacing *= explorer_factor;
                    ui.style_mut().spacing.button_padding *= explorer_factor;
                    ui.style_mut().spacing.interact_size *= explorer_factor;
                    for font in ui.style_mut().text_styles.values_mut() {
                        font.size *= explorer_factor;
                    }
                    let mut actions_inline = false;
                    ui.horizontal(|ui| {
                        for (tab, icon, help) in [
                            (
                                SidebarTab::Library,
                                RailIcon::Files,
                                "Projects and recent PDFs",
                            ),
                            (SidebarTab::Contents, RailIcon::Contents, "Document outline"),
                            (SidebarTab::Search, RailIcon::Search, "Find (Ctrl+F)"),
                        ] {
                            if rail_button(ui, self.sidebar_tab == tab, icon)
                                .on_hover_text(help)
                                .clicked()
                            {
                                if tab == SidebarTab::Search {
                                    self.open_search();
                                } else {
                                    self.sidebar_tab = tab;
                                    self.show_search = false;
                                }
                            }
                        }
                        // Library actions share the tab row when there is room
                        // and move to their own row in a narrow sidebar.
                        actions_inline = self.sidebar_tab == SidebarTab::Library
                            && ui.available_width() >= 3.0 * 36.0 * sidebar_visual_scale(ui);
                        if actions_inline {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if chrome_button(
                                    ui,
                                    ToolbarIcon::Refresh,
                                    false,
                                    true,
                                    "Rescan project folders",
                                )
                                .clicked()
                                {
                                    self.refresh_projects();
                                }
                                if chrome_button(
                                    ui,
                                    ToolbarIcon::FolderPlus,
                                    false,
                                    true,
                                    "Add a project folder",
                                )
                                .clicked()
                                {
                                    self.add_project_dialog();
                                }
                                if chrome_button(
                                    ui,
                                    ToolbarIcon::OpenFile,
                                    false,
                                    true,
                                    "Open a PDF (Ctrl+O)",
                                )
                                .clicked()
                                {
                                    self.open_file_dialog();
                                }
                            });
                        }
                    });
                    if self.sidebar_tab == SidebarTab::Library && !actions_inline {
                        ui.horizontal(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if chrome_button(
                                    ui,
                                    ToolbarIcon::Refresh,
                                    false,
                                    true,
                                    "Rescan project folders",
                                )
                                .clicked()
                                {
                                    self.refresh_projects();
                                }
                                if chrome_button(
                                    ui,
                                    ToolbarIcon::FolderPlus,
                                    false,
                                    true,
                                    "Add a project folder",
                                )
                                .clicked()
                                {
                                    self.add_project_dialog();
                                }
                                if chrome_button(
                                    ui,
                                    ToolbarIcon::OpenFile,
                                    false,
                                    true,
                                    "Open a PDF (Ctrl+O)",
                                )
                                .clicked()
                                {
                                    self.open_file_dialog();
                                }
                            });
                        });
                    }
                    ui.add_space(6.0);
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
                                                            &self
                                                                .sidebar_search
                                                                .trim()
                                                                .to_lowercase(),
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
                                        register_rename_row(
                                            ui,
                                            &RenameTarget::Project(index),
                                            header.rename_rect,
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
                                            &header.body,
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
                                                        header.rename_rect,
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
                                            let tree =
                                                build_pdf_tree(&project.root, &matching_files);
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
                                                    TreeAction::Rename(path, name, rect) => {
                                                        rename_request = Some((
                                                            RenameTarget::File(path),
                                                            name,
                                                            rect,
                                                        ));
                                                    }
                                                    TreeAction::ResetName(path) => {
                                                        clear_file_alias = Some(path)
                                                    }
                                                    TreeAction::PinFile(path) => {
                                                        pin_file = Some(path)
                                                    }
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
                                            &row.body,
                                            false,
                                            None,
                                            false,
                                            true,
                                        ) {
                                            match action {
                                                SidebarMenuAction::Unpin => {
                                                    unpin_folder = Some(folder)
                                                }
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
                                    register_rename_row(
                                        ui,
                                        &RenameTarget::File(path.clone()),
                                        row.rename_rect,
                                    );
                                    if row.body.clicked() && !row.options.clicked() {
                                        open_after_ui = Some(path.clone());
                                    }
                                    if let Some(action) = sidebar_options_popup(
                                        &row.options,
                                        &row.body,
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
                                                    row.rename_rect,
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
                            let has_unpinned_projects =
                                self.state.projects.iter().any(|project| {
                                    !self.state.pinned_folders.contains(&project.root)
                                });
                            if has_unpinned_projects || self.state.projects.is_empty() {
                                sidebar_section_header(ui, "PROJECTS", "", theme::MUTED);
                            }
                            if self.state.projects.is_empty() {
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new(
                                        "Add a course or research folder to browse its PDFs.",
                                    )
                                    .color(theme::MUTED)
                                    .size(13.0 * explorer_factor),
                                );
                                ui.add_space(6.0);
                                if ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new("Add folder")
                                                .size(13.5 * explorer_factor)
                                                .color(theme::ON_ACCENT),
                                        )
                                        .fill(theme::ACCENT_BLUE)
                                        .corner_radius(CornerRadius::same(7)),
                                    )
                                    .clicked()
                                {
                                    self.add_project_dialog();
                                }
                                ui.add_space(4.0);
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
                                            || self
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

                                ui.add_space(3.0);
                                let header = project_header(
                                    ui,
                                    &project.name,
                                    matching_files.len(),
                                    self.state.projects[index].expanded,
                                );
                                register_rename_row(
                                    ui,
                                    &RenameTarget::Project(index),
                                    header.rename_rect,
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
                                    &header.body,
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
                                                header.rename_rect,
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
                                            TreeAction::OpenInFiles(path) => {
                                                open_in_files = Some(path)
                                            }
                                            TreeAction::Rename(path, name, rect) => {
                                                rename_request =
                                                    Some((RenameTarget::File(path), name, rect));
                                            }
                                            TreeAction::ResetName(path) => {
                                                clear_file_alias = Some(path)
                                            }
                                            TreeAction::PinFile(path) => pin_file = Some(path),
                                            TreeAction::UnpinFile(path) => unpin_file = Some(path),
                                            TreeAction::PinFolder(path) => pin_folder = Some(path),
                                            TreeAction::UnpinFolder(path) => {
                                                unpin_folder = Some(path)
                                            }
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
                                    register_rename_row(
                                        ui,
                                        &RenameTarget::File(item.path.clone()),
                                        row.rename_rect,
                                    );
                                    if row.truncated {
                                        row.body
                                            .clone()
                                            .on_hover_text(self.state.display_name(&item.path));
                                    }
                                    if row.body.clicked() && !row.options.clicked() {
                                        open_after_ui = Some(item.path.clone());
                                    }
                                    if let Some(action) = sidebar_options_popup(
                                        &row.options,
                                        &row.body,
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
                                                    row.rename_rect,
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
                            if let Some((target, current_name, anchor)) = rename_request {
                                self.rename_target = Some(target);
                                self.rename_anchor = Some(anchor);
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
        let scale = toolbar_visual_scale(ui);
        let documents = self.state.open_documents.clone();
        let mut activate = None;
        let mut close = None;
        let tab_height = 32.0 * scale;
        // Bottom-align so the active tab merges into the address bar and the
        // strip's buttons sit level with the tab labels.
        ui.with_layout(Layout::left_to_right(Align::Max), |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            // Edge keeps its tab actions at the far left of the strip; here that
            // slot toggles the project/outline sidebar.
            if chrome_button(ui, ToolbarIcon::Sidebar, false, true, "Toggle sidebar").clicked() {
                self.sidebar_visible = !self.sidebar_visible;
            }
            ui.add_space(6.0 * scale);
            let corner_buttons = if self.state.hide_study_chat { 1.0 } else { 2.0 };
            let new_tab_width = 34.0 * scale + corner_buttons * 32.0 * scale + 12.0 * scale;
            let tab_width = ((ui.available_width() - new_tab_width)
                / documents.len().max(1) as f32)
                .clamp(44.0 * scale, 236.0 * scale);
            {
                let active_index = documents
                    .iter()
                    .position(|path| self.current_path.as_deref() == Some(path.as_path()));
                for (index, path) in documents.iter().enumerate() {
                    let active = active_index == Some(index);
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(tab_width, tab_height), Sense::click());
                    let close_rect = Rect::from_center_size(
                        pos2(rect.right() - 16.0 * scale, rect.center().y),
                        vec2(20.0, 20.0) * scale,
                    );
                    let close_response = ui
                        .interact(
                            close_rect,
                            ui.id().with(("tab-close", index)),
                            Sense::click(),
                        )
                        .on_hover_text("Close tab");
                    let painter = ui.painter();
                    if active {
                        painter.rect_filled(
                            rect,
                            CornerRadius {
                                nw: 8,
                                ne: 8,
                                sw: 0,
                                se: 0,
                            },
                            theme::TOOLBAR_BG,
                        );
                    } else if response.hovered() || close_response.hovered() {
                        painter.rect_filled(
                            rect.shrink2(vec2(2.0, 3.0) * scale),
                            CornerRadius::same(6),
                            Color32::from_rgb(40, 40, 40),
                        );
                    } else if active_index != Some(index + 1) && index + 1 < documents.len() {
                        painter.line_segment(
                            [
                                pos2(rect.right(), rect.top() + 9.0 * scale),
                                pos2(rect.right(), rect.bottom() - 9.0 * scale),
                            ],
                            Stroke::new(1.0, Color32::from_rgb(64, 64, 64)),
                        );
                    }
                    let text_left = rect.left() + 12.0 * scale;
                    let text_width = (close_rect.left() - 4.0 * scale - text_left).max(0.0);
                    let mut job = egui::text::LayoutJob::single_section(
                        self.state.display_name(path),
                        egui::TextFormat {
                            font_id: FontId::proportional(12.5 * scale),
                            color: if active {
                                Color32::from_rgb(240, 240, 240)
                            } else {
                                Color32::from_rgb(170, 170, 170)
                            },
                            ..Default::default()
                        },
                    );
                    job.wrap = egui::text::TextWrapping {
                        max_width: text_width,
                        max_rows: 1,
                        break_anywhere: true,
                        overflow_character: Some('…'),
                    };
                    let galley = painter.layout_job(job);
                    painter.galley(
                        pos2(text_left, rect.center().y - galley.size().y / 2.0),
                        galley,
                        Color32::WHITE,
                    );
                    if active || response.hovered() || close_response.hovered() {
                        if close_response.hovered() {
                            painter.circle_filled(
                                close_rect.center(),
                                9.0 * scale,
                                Color32::from_rgb(64, 64, 64),
                            );
                        }
                        paint_toolbar_icon(
                            painter,
                            close_rect.center(),
                            ToolbarIcon::Close,
                            Color32::from_rgb(200, 200, 200),
                        );
                    }
                    if close_response.clicked() || response.middle_clicked() {
                        close = Some(path.clone());
                    } else if response.clicked() {
                        activate = Some(path.clone());
                    }
                    response.on_hover_text(path.display().to_string());
                }
            }
            if chrome_button(ui, ToolbarIcon::Plus, false, true, "Open PDF (Ctrl+O)").clicked() {
                self.open_file_dialog();
            }
            // Window-level actions live in the strip's right corner, as in Edge.
            ui.with_layout(Layout::right_to_left(Align::Max), |ui| {
                if chrome_button(ui, ToolbarIcon::Settings, false, true, "Settings").clicked() {
                    self.pdf_default_application = current_pdf_default_application();
                    self.show_view_settings = true;
                }
                if !self.state.hide_study_chat
                    && chrome_button(
                        ui,
                        ToolbarIcon::Chat,
                        self.study_chat.visible,
                        true,
                        // While the panel is open, the tooltip would cover its header.
                        if self.study_chat.visible {
                            ""
                        } else {
                            "Study chat (Ctrl+Shift+A)"
                        },
                    )
                    .clicked()
                {
                    self.study_chat.visible = !self.study_chat.visible;
                }
            });
        });
        if let Some(path) = close {
            self.close_document_tab(&path);
        } else if let Some(path) = activate {
            self.open_document(path);
        }
    }

    /// Show the search sidebar with the cursor in its field, ready for typing.
    fn open_search(&mut self) {
        self.sidebar_visible = true;
        self.show_search = true;
        self.sidebar_tab = SidebarTab::Search;
        self.search_focus_requested = true;
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
        let search_field_id = egui::Id::new("sidebar-search-field");
        if std::mem::take(&mut self.search_focus_requested) {
            // Select the previous query so typing replaces it, as in a browser.
            let mut state =
                egui::TextEdit::load_state(ui.ctx(), search_field_id).unwrap_or_default();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(self.search_query.chars().count()),
                )));
            state.store(ui.ctx(), search_field_id);
            ui.ctx()
                .memory_mut(|memory| memory.request_focus(search_field_id));
        }
        let response = ui.add_sized(
            [ui.available_width(), 32.0],
            egui::TextEdit::singleline(&mut self.search_query)
                .id(search_field_id)
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

    /// Browser-like chrome modelled on Edge's PDF viewer: a tab strip, an
    /// address bar holding the path, and one centered document toolbar.
    fn toolbar(&mut self, root: &mut egui::Ui) {
        if !self.toolbar_visible || self.reader_mode || self.presentation_mode {
            return;
        }
        let toolbar_factor = interface_region_scale(self.state.chrome_scale_ratio);
        egui::Panel::top("tab-strip")
            .frame(Frame::new().fill(theme::TAB_STRIP_BG).inner_margin(Margin {
                left: 6,
                right: 6,
                top: 5,
                bottom: 0,
            }))
            .show(root, |ui| {
                apply_region_scale(ui, toolbar_factor);
                self.document_tabs(ui);
            });

        if self.state.path_bar == PathBarStyle::Bar {
            egui::Panel::top("address-bar")
                .frame(Frame::new().fill(theme::TOOLBAR_BG).inner_margin(Margin {
                    left: 8,
                    right: 8,
                    top: 5,
                    bottom: 4,
                }))
                .show(root, |ui| {
                    apply_region_scale(ui, toolbar_factor);
                    self.address_bar(ui);
                });
        }

        if self.current_path.is_none() {
            return;
        }
        egui::Panel::top("toolbar")
            .frame(
                Frame::new()
                    .fill(theme::TOOLBAR_BG)
                    .inner_margin(Margin::symmetric(8, 1)),
            )
            .show(root, |ui| {
                apply_region_scale(ui, toolbar_factor);
                let scale = toolbar_visual_scale(ui);
                let row =
                    Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), 26.0 * scale));
                ui.allocate_rect(row, Sense::hover());
                let center_width = (300.0 * scale).min(row.width() / 3.0);
                let center = Rect::from_center_size(row.center(), vec2(center_width, row.height()));
                let left = Rect::from_min_max(row.min, pos2(center.left(), row.bottom()));
                let right = Rect::from_min_max(pos2(center.right(), row.top()), row.max);
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(center).layout(
                        Layout::left_to_right(Align::Center).with_main_align(Align::Center),
                    ),
                    |ui| self.toolbar_page_controls(ui),
                );
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(left)
                        .layout(Layout::left_to_right(Align::Center)),
                    |ui| {
                        ScrollArea::horizontal()
                            .id_salt("viewer-editing-tools-scroll")
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                            )
                            .show(ui, |ui| {
                                ui.allocate_ui_with_layout(
                                    vec2(ui.available_width(), row.height()),
                                    Layout::left_to_right(Align::Center),
                                    |ui| {
                                        if self.state.path_bar == PathBarStyle::InToolbar {
                                            self.path_crumb(ui);
                                            toolbar_divider(ui);
                                        }
                                        self.toolbar_editing_tools(ui);
                                    },
                                );
                            });
                    },
                );
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(right)
                        .layout(Layout::right_to_left(Align::Center)),
                    |ui| self.toolbar_document_actions(ui),
                );
            });
    }

    /// The full-width path row: every folder is clickable, the file name
    /// copies the path, and the chevron collapses it into the toolbar.
    fn address_bar(&mut self, ui: &mut egui::Ui) {
        let scale = toolbar_visual_scale(ui);
        let breadcrumb_path = self
            .browser_directory
            .as_deref()
            .or(self.current_path.as_deref())
            .map(Path::to_path_buf);
        let mut breadcrumb_action = None;
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if chrome_button(
                    ui,
                    ToolbarIcon::CollapseUp,
                    false,
                    true,
                    "Collapse the path into the toolbar",
                )
                .clicked()
                {
                    self.state.path_bar = PathBarStyle::InToolbar;
                    self.persist();
                }
                ui.add_space(4.0 * scale);
                Frame::new()
                    .fill(theme::ADDRESS_BG)
                    .corner_radius(CornerRadius::same(15))
                    .inner_margin(Margin::symmetric(6, 0))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.set_height(28.0 * scale);
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            if chrome_button(
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
                            ui.label(RichText::new("File").size(12.5 * scale).color(theme::MUTED));
                            toolbar_divider(ui);
                            ScrollArea::horizontal()
                                .id_salt("path-actions-scroll")
                                .auto_shrink([false, true])
                                .scroll_bar_visibility(
                                    egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                                )
                                .scroll_source(egui::scroll_area::ScrollSource::ALL)
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 2.0 * scale;
                                        breadcrumb_action =
                                            path_breadcrumbs(ui, breadcrumb_path.as_deref());
                                    });
                                });
                        });
                    });
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
    }

    /// The document's folder as a compact crumb at the left of the toolbar.
    /// Its menu holds the full clickable path, copying and Files.
    fn path_crumb(&mut self, ui: &mut egui::Ui) {
        let Some(path) = self
            .browser_directory
            .clone()
            .or_else(|| self.current_path.clone())
        else {
            return;
        };
        let folder = if path.is_dir() {
            path.clone()
        } else {
            path.parent().map(Path::to_path_buf).unwrap_or_default()
        };
        let names: Vec<String> = folder
            .components()
            .filter_map(|component| match component {
                std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        let tail = names[names.len().saturating_sub(2)..].join(" / ");
        let label = if names.len() > 2 {
            format!("… / {tail}")
        } else {
            tail
        };

        let scale = toolbar_visual_scale(ui);
        let font = FontId::proportional(12.5 * scale);
        let text_width = ui
            .painter()
            .layout_no_wrap(label.clone(), font.clone(), theme::MUTED)
            .size()
            .x
            .min(190.0 * scale);
        let height = (30.0 * scale).min(ui.max_rect().height());
        let (rect, response) =
            ui.allocate_exact_size(vec2(text_width + 44.0 * scale, height), Sense::click());
        let hovered = response.hovered();
        if hovered {
            ui.painter().rect_filled(
                rect.shrink(1.0),
                CornerRadius::same(6),
                Color32::from_rgb(54, 54, 54),
            );
        }
        let color = if hovered {
            Color32::from_rgb(235, 235, 235)
        } else {
            Color32::from_rgb(200, 200, 200)
        };
        paint_folder_icon(
            ui.painter(),
            pos2(rect.left() + 13.0 * scale, rect.center().y),
            color,
            0.9 * scale,
        );
        paint_elided_label(
            ui.painter(),
            pos2(rect.left() + 25.0 * scale, rect.center().y),
            &label,
            text_width,
            font,
            color,
        );
        let chevron = pos2(rect.right() - 10.0 * scale, rect.center().y + 1.0);
        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                chevron + vec2(-3.5, -2.0) * scale,
                chevron + vec2(3.5, -2.0) * scale,
                chevron + vec2(0.0, 2.5) * scale,
            ],
            color,
            Stroke::NONE,
        ));
        let response = response.on_hover_text(path.display().to_string());

        let mut action = None;
        let mut show_in_files = false;
        egui::Popup::menu(&response).show(|ui| {
            ui.set_min_width(280.0);
            let mut cumulative = PathBuf::from("/");
            for (depth, name) in names.iter().enumerate() {
                cumulative.push(name);
                let clicked = ui
                    .horizontal(|ui| {
                        ui.add_space(depth.min(10) as f32 * 10.0);
                        ui.selectable_label(cumulative == folder, name.as_str())
                            .on_hover_text("Browse PDFs in this folder")
                            .clicked()
                    })
                    .inner;
                if clicked {
                    action = Some(BreadcrumbAction::Browse(cumulative.clone()));
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("Copy full path").clicked() {
                action = Some(BreadcrumbAction::Copy(path.clone()));
                ui.close();
            }
            if ui.button("Show in Files").clicked() {
                show_in_files = true;
                ui.close();
            }
            if ui.button("Show the full path bar").clicked() {
                self.state.path_bar = PathBarStyle::Bar;
                self.persist();
                ui.close();
            }
        });
        match action {
            Some(BreadcrumbAction::Browse(directory)) => self.open_folder_browser(directory),
            Some(BreadcrumbAction::Copy(path)) => {
                ui.ctx().copy_text(path.display().to_string());
                self.status = "Path copied.".to_owned();
            }
            None => {}
        }
        if show_in_files {
            self.open_current_directory();
        }
    }
    fn toolbar_page_controls(&mut self, ui: &mut egui::Ui) {
        let scale = toolbar_visual_scale(ui);
        ui.spacing_mut().item_spacing.x = 2.0 * scale;
        // Document zoom is independent from every interface scale.
        if chrome_button(ui, ToolbarIcon::ZoomOut, false, true, "Zoom out (Ctrl+-)").clicked() {
            let cursor = ui.ctx().input(|input| input.pointer.hover_pos());
            self.apply_pinch_zoom(1.0 / 1.1, cursor, ui.ctx());
        }
        if chrome_button(ui, ToolbarIcon::ZoomIn, false, true, "Zoom in (Ctrl++)").clicked() {
            let cursor = ui.ctx().input(|input| input.pointer.hover_pos());
            self.apply_pinch_zoom(1.1, cursor, ui.ctx());
        }
        if chrome_button(
            ui,
            ToolbarIcon::FitWidth,
            false,
            true,
            &format!("Fit to width (now {}%)", (self.zoom * 100.0).round() as i32),
        )
        .clicked()
        {
            self.fit_width_requested = true;
        }
        toolbar_divider(ui);
        if self.page_sizes.is_empty() {
            return;
        }
        if !self.page_entry_editing {
            self.page_entry = (self.current_page + 1).to_string();
        }
        let response = ui.add_sized(
            [38.0 * scale, 22.0 * scale],
            egui::TextEdit::singleline(&mut self.page_entry)
                .id(egui::Id::new(PAGE_ENTRY_ID))
                .horizontal_align(Align::Center)
                .vertical_align(Align::Center)
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
            && !self
                .page_entry
                .trim()
                .parse::<usize>()
                .is_ok_and(|page| (1..=self.page_sizes.len()).contains(&page))
        {
            self.page_entry = (self.current_page + 1).to_string();
        }
        ui.add_space(4.0 * scale);
        ui.label(
            RichText::new(format!("of {}", self.page_sizes.len()))
                .size(13.0 * scale)
                .color(theme::MUTED),
        );
    }

    fn toolbar_editing_tools(&mut self, ui: &mut egui::Ui) {
        let scale = toolbar_visual_scale(ui);
        ui.spacing_mut().item_spacing.x = 2.0 * scale;
        toolbar_tool_button(
            ui,
            &mut self.tool_mode,
            ToolMode::Highlight,
            ToolbarIcon::Highlight,
            "Highlight text",
        );
        toolbar_tool_button(
            ui,
            &mut self.tool_mode,
            ToolMode::Pen,
            ToolbarIcon::Pen,
            "Draw",
        );
        toolbar_tool_button(
            ui,
            &mut self.tool_mode,
            ToolMode::Eraser,
            ToolbarIcon::Eraser,
            "Erase annotations",
        );
        toolbar_tool_button(
            ui,
            &mut self.tool_mode,
            ToolMode::Note,
            ToolbarIcon::Comment,
            "Add comment",
        );
        let more_tools = chrome_button(
            ui,
            ToolbarIcon::More,
            matches!(
                self.tool_mode,
                ToolMode::TextBox
                    | ToolMode::Rectangle
                    | ToolMode::Ellipse
                    | ToolMode::Signature
                    | ToolMode::Stamp
                    | ToolMode::Attachment
            ),
            true,
            "More annotation tools",
        );
        egui::Popup::menu(&more_tools).show(|ui| {
            for (mode, label, help) in [
                (
                    ToolMode::TextBox,
                    "Text box",
                    "Place editable text directly on the page",
                ),
                (ToolMode::Rectangle, "Rectangle", "Drag a rectangle"),
                (ToolMode::Ellipse, "Ellipse", "Drag an ellipse"),
                (
                    ToolMode::Signature,
                    "Signature",
                    "Draw a signature with a black pen",
                ),
                (ToolMode::Stamp, "Stamp", "Place a custom text stamp"),
                (
                    ToolMode::Attachment,
                    "Attach file",
                    "Attach a local file reference to the page",
                ),
            ] {
                tool_button(ui, &mut self.tool_mode, mode, label, help);
            }
        });
        annotation_color_dot(ui, &mut self.annotation_color);
        toolbar_divider(ui);
        toolbar_tool_button(
            ui,
            &mut self.tool_mode,
            ToolMode::Ocr,
            ToolbarIcon::Ocr,
            "Snip OCR",
        );
        toolbar_tool_button(
            ui,
            &mut self.tool_mode,
            ToolMode::MathOcr,
            ToolbarIcon::Math,
            "Math OCR (experimental)",
        );
        toolbar_divider(ui);
        if chrome_button(
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
        if chrome_button(
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
    }

    /// Right-hand toolbar cluster, laid out right-to-left.
    fn toolbar_document_actions(&mut self, ui: &mut egui::Ui) {
        let scale = toolbar_visual_scale(ui);
        ui.spacing_mut().item_spacing.x = 2.0 * scale;
        let more = chrome_button(ui, ToolbarIcon::More, false, true, "More actions");
        egui::Popup::menu(&more).show(|ui| {
            if ui.button("Open with another app…").clicked()
                && let Some(path) = self.current_path.clone()
            {
                self.request_open_with(path);
            }
            if ui.button("Hide toolbar").clicked() {
                self.toolbar_visible = false;
            }
        });
        if chrome_button(
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
        if chrome_button(
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
        toolbar_divider(ui);
        if chrome_button(
            ui,
            ToolbarIcon::Export,
            false,
            true,
            "Save annotated copy as…",
        )
        .clicked()
        {
            self.export_annotated_copy();
        }
        let has_annotations = !self.current_annotations().is_empty();
        if chrome_button(
            ui,
            ToolbarIcon::Save,
            false,
            has_annotations,
            "Save annotations into this PDF",
        )
        .clicked()
            && has_annotations
        {
            self.confirm_save_to_source = true;
        }
        if chrome_button(ui, ToolbarIcon::Print, false, true, "Print (Ctrl+P)").clicked() {
            self.request_print();
        }
        toolbar_divider(ui);
        let searching = self.sidebar_visible && self.sidebar_tab == SidebarTab::Search;
        if chrome_button(ui, ToolbarIcon::Search, searching, true, "Find (Ctrl+F)").clicked() {
            if searching {
                self.sidebar_visible = false;
                self.show_search = false;
            } else {
                self.open_search();
            }
        }
    }

    fn add_chat_attachment(&mut self, path: PathBuf) {
        if self.state.hide_study_chat {
            return;
        }
        const MAX_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;
        if self.study_chat.attachments.len() >= 6 {
            self.study_chat.status =
                "Send or remove an attachment before adding another.".to_owned();
            return;
        }
        let Some(mime) = mime_for_path(&path) else {
            self.study_chat.status =
                "Attach a PDF, image, or plain-text file (PDF, PNG, JPEG, WebP, GIF, TXT, MD, TEX, CSV)."
                    .to_owned();
            return;
        };
        match fs::metadata(&path) {
            Ok(metadata) if metadata.len() > MAX_ATTACHMENT_BYTES => {
                self.study_chat.status =
                    "That attachment is larger than SnaPdf's 25 MB safety limit.".to_owned();
                return;
            }
            Err(error) => {
                self.study_chat.status = format!("Could not read attachment: {error}");
                return;
            }
            _ => {}
        }
        match fs::read(&path) {
            Ok(bytes) => {
                self.study_chat.attachments.push(ChatAttachment {
                    name: file_name(&path),
                    mime: mime.to_owned(),
                    bytes,
                });
                self.study_chat.visible = true;
                self.study_chat.status =
                    "Attachment ready. It is shared only when you send.".to_owned();
            }
            Err(error) => {
                self.study_chat.status = format!("Could not read attachment: {error}");
            }
        }
    }

    fn choose_chat_attachment(&mut self) {
        if let Some(paths) = rfd::FileDialog::new()
            .add_filter(
                "Study material",
                &[
                    "pdf", "png", "jpg", "jpeg", "webp", "gif", "txt", "md", "tex", "csv",
                ],
            )
            .pick_files()
        {
            for path in paths {
                if self.study_chat.attachments.len() >= 6 {
                    break;
                }
                self.add_chat_attachment(path);
            }
        }
    }

    fn send_chat_message(&mut self) {
        let provider = self.study_chat.provider;
        if !self
            .study_chat
            .connected
            .get(&provider)
            .copied()
            .unwrap_or(false)
        {
            self.study_chat.status = format!("Connect {} before sending.", provider.label());
            return;
        }
        let Some(model) = self.study_chat.selected_models.get(&provider).cloned() else {
            self.study_chat.status = "Choose a model before sending.".to_owned();
            return;
        };
        let reasoning_effort = (provider == ChatProvider::ChatGpt
            && model_supports_reasoning_effort(&model))
        .then_some(self.state.chat_reasoning_effort);
        if self.study_chat.busy {
            return;
        }
        let text = self.study_chat.prompt.trim().to_owned();
        if text.is_empty() && self.study_chat.attachments.is_empty() {
            self.study_chat.status = "Type a question or add study material first.".to_owned();
            return;
        }
        let user_message = ChatMessage {
            role: ChatRole::User,
            text,
            attachments: std::mem::take(&mut self.study_chat.attachments),
        };
        self.study_chat.prompt.clear();
        {
            let session = self.study_chat.active_session_mut();
            if session.title == "New chat" && !user_message.text.trim().is_empty() {
                session.title = normalized_chat_title(&user_message.text)
                    .unwrap_or_else(|| "New chat".to_owned());
            }
            session.last_opened = unix_timestamp();
        }
        let conversation = {
            let messages = self
                .study_chat
                .active_session_mut()
                .messages
                .entry(provider)
                .or_default();
            messages.push(user_message);
            let conversation = messages.clone();
            messages.push(ChatMessage {
                role: ChatRole::Assistant,
                text: String::new(),
                attachments: Vec::new(),
            });
            conversation
        };
        let request_id = self.study_chat.next_request;
        self.study_chat.next_request = request_id.wrapping_add(1);
        self.study_chat.active_request = Some(request_id);
        self.study_chat.active_provider = Some(provider);
        self.study_chat.busy = true;
        self.study_chat.status = format!("{} is thinking…", provider.label());
        if self
            .chat_worker
            .commands
            .send(ChatCommand::Send {
                request_id,
                provider,
                model,
                reasoning_effort,
                messages: conversation,
            })
            .is_err()
        {
            self.study_chat.busy = false;
            self.study_chat.active_request = None;
            self.study_chat.active_provider = None;
            self.study_chat.status = "Study chat worker stopped unexpectedly.".to_owned();
        }
    }

    fn study_chat_compact_controls(
        &mut self,
        ui: &mut egui::Ui,
        provider: ChatProvider,
        connected: bool,
        text_size: f32,
    ) {
        if connected {
            let models = self
                .study_chat
                .models
                .get(&provider)
                .cloned()
                .unwrap_or_default();
            let selected_model = self
                .study_chat
                .selected_models
                .entry(provider)
                .or_insert_with(|| models.first().cloned().unwrap_or_default())
                .clone();
            let reasoning_supported = provider == ChatProvider::ChatGpt
                && model_supports_reasoning_effort(&selected_model);
            // Models are listed only after sign-in; before that a permanent
            // "Loading…" would look stuck.
            let model_text = if !selected_model.is_empty() {
                selected_model.as_str()
            } else if connected {
                "Loading…"
            } else {
                "Model"
            };
            let (model_width, thinking_width, spacer) =
                chat_model_control_widths(ui.available_width(), reasoning_supported);
            let mut chosen_model = None;
            let mut chosen_effort = None;

            if reasoning_supported {
                let response = chat_dropdown_button(
                    ui,
                    self.state.chat_reasoning_effort.label(),
                    thinking_width,
                    false,
                    "Choose thinking level",
                    text_size,
                );
                egui::Popup::menu(&response).show(|ui| {
                    for effort in [
                        ReasoningEffort::Instant,
                        ReasoningEffort::Medium,
                        ReasoningEffort::High,
                    ] {
                        if ui
                            .selectable_label(
                                effort == self.state.chat_reasoning_effort,
                                effort.label(),
                            )
                            .clicked()
                        {
                            chosen_effort = Some(effort);
                            ui.close();
                        }
                    }
                });
            }

            let response = chat_dropdown_button(
                ui,
                model_text,
                model_width,
                false,
                "Choose model",
                text_size,
            );
            egui::Popup::menu(&response).show(|ui| {
                ui.set_min_width(model_width.max(132.0));
                for model in &models {
                    if ui
                        .selectable_label(*model == selected_model, model)
                        .clicked()
                    {
                        chosen_model = Some(model.clone());
                        ui.close();
                    }
                }
            });

            ui.add_space(spacer);

            if let Some(model) = chosen_model {
                self.study_chat.selected_models.insert(provider, model);
            }
            if let Some(effort) = chosen_effort {
                self.state.chat_reasoning_effort = effort;
                self.persist();
            }
        } else {
            let connect = chat_action_button(
                ui,
                "Connect",
                72.0_f32.min(ui.available_width().max(1.0)),
                "Connect the selected AI provider",
                text_size,
            );
            if connect.clicked() {
                match provider {
                    ChatProvider::ChatGpt => {
                        self.study_chat.status = "Opening secure ChatGPT sign-in…".to_owned();
                        let _ = self.chat_worker.commands.send(ChatCommand::SignInChatGpt);
                    }
                    ChatProvider::Gemini => {
                        self.study_chat.show_gemini_key = true;
                    }
                }
            }
        }
    }

    fn study_chat_panel(&mut self, root: &mut egui::Ui) {
        if self.state.hide_study_chat
            || !self.study_chat.visible
            || self.reader_mode
            || self.presentation_mode
        {
            return;
        }
        egui::Panel::right("study-chat")
            .resizable(true)
            .default_size(430.0)
            .min_size(0.0)
            .max_size(f32::INFINITY)
            .frame(
                Frame::new()
                    .fill(CHAT_CANVAS)
                    .stroke(Stroke::new(1.0, CHAT_DIVIDER))
                    .inner_margin(Margin::ZERO),
            )
            .show(root, |ui| {
                bounded_panel_content(ui, "study-chat-content", |ui| {
                    let chat_font_size = self.state.chat_font_size;
                    apply_chat_typography(ui, chat_font_size);
                    let provider = self.study_chat.provider;

                    Frame::new()
                        .fill(CHAT_HEADER)
                        .inner_margin(Margin::symmetric(16, 11))
                        .show(ui, |ui| {
                            let mut begin_rename = false;
                            let mut chosen_provider = None;
                            let account_connected = self
                                .study_chat
                                .connected
                                .get(&provider)
                                .copied()
                                .unwrap_or(false);
                            let account_name = self
                                .study_chat
                                .accounts
                                .get(&provider)
                                .cloned()
                                .unwrap_or_else(|| "Not signed in".to_owned());
                            ui.horizontal(|ui| {
                                let provider_menu = chat_header_provider_button(
                                    ui,
                                    provider.label(),
                                    chat_font_size,
                                    "Choose AI provider",
                                );
                                egui::Popup::menu(&provider_menu).show(|ui| {
                                    for candidate in
                                        [ChatProvider::ChatGpt, ChatProvider::Gemini]
                                    {
                                        if ui
                                            .selectable_label(
                                                candidate == provider,
                                                candidate.label(),
                                            )
                                            .clicked()
                                        {
                                            chosen_provider = Some(candidate);
                                            ui.close();
                                        }
                                    }
                                });
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if chat_icon_button(ui, ToolbarIcon::Close, true, "Close study chat").clicked() {
                                        self.study_chat.visible = false;
                                        self.persist();
                                    }
                                    let more = chat_icon_button(
                                        ui,
                                        ToolbarIcon::More,
                                        true,
                                        "Account",
                                    );
                                    egui::Popup::menu(&more).show(|ui| {
                                        ui.set_min_width(220.0);
                                        ui.set_min_width(220.0);
                                        ui.label(
                                            RichText::new(provider.label())
                                                .size(chat_control_text_size(chat_font_size))
                                                .strong(),
                                        );
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(&account_name)
                                                    .size(chat_meta_text_size(chat_font_size))
                                                    .color(CHAT_MUTED),
                                            )
                                            .wrap(),
                                        );
                                        ui.add_space(5.0);
                                        if account_connected {
                                            if ui.button("Sign out").clicked() {
                                                let _ = self
                                                    .chat_worker
                                                    .commands
                                                    .send(ChatCommand::SignOut(provider));
                                                self.study_chat.status = format!(
                                                    "Signed out of {}.",
                                                    provider.label()
                                                );
                                                ui.close();
                                            }
                                            match provider {
                                                ChatProvider::ChatGpt => {
                                                    if ui.button("Sign in with another account").clicked() {
                                                        let _ = self
                                                            .chat_worker
                                                            .commands
                                                            .send(ChatCommand::SignOut(provider));
                                                        let _ = self
                                                            .chat_worker
                                                            .commands
                                                            .send(ChatCommand::SignInChatGpt);
                                                        self.study_chat.status =
                                                            "Opening secure ChatGPT sign-in…".to_owned();
                                                        ui.close();
                                                    }
                                                }
                                                ChatProvider::Gemini => {
                                                    if ui.button("Replace API key").clicked() {
                                                        self.study_chat.show_gemini_key = true;
                                                        ui.close();
                                                    }
                                                }
                                            }
                                        } else {
                                            match provider {
                                                ChatProvider::ChatGpt => {
                                                    if ui.button("Continue with ChatGPT").clicked() {
                                                        self.study_chat.status =
                                                            "Opening secure ChatGPT sign-in…".to_owned();
                                                        let _ = self
                                                            .chat_worker
                                                            .commands
                                                            .send(ChatCommand::SignInChatGpt);
                                                        ui.close();
                                                    }
                                                }
                                                ChatProvider::Gemini => {
                                                    if ui.button("Connect Gemini API key").clicked() {
                                                        self.study_chat.show_gemini_key = true;
                                                        ui.close();
                                                    }
                                                }
                                            }
                                        }
                                    });
                                    if chat_icon_button(
                                        ui,
                                        ToolbarIcon::NewChat,
                                        !self.study_chat.busy,
                                        "New study chat",
                                    )
                                    .clicked()
                                    {
                                        self.request_new_study_chat();
                                    }
                                    if chat_icon_button(
                                        ui,
                                        ToolbarIcon::DeleteChat,
                                        !self.study_chat.busy,
                                        "Delete this chat",
                                    )
                                    .clicked()
                                    {
                                        self.delete_active_study_chat();
                                    }
                                    if chat_icon_button(
                                        ui,
                                        ToolbarIcon::Rename,
                                        !self.study_chat.busy,
                                        "Rename this chat",
                                    )
                                    .clicked()
                                    {
                                        begin_rename = true;
                                    }
                                });
                            });
                            if let Some(provider) = chosen_provider {
                                self.study_chat.provider = provider;
                            }
                            if begin_rename {
                                self.begin_study_chat_rename();
                            }
                            ui.add_space(5.0);
                            if self.study_chat.renaming_session
                                == Some(self.study_chat.active_session_id)
                            {
                                let mut commit = false;
                                let mut cancel = false;
                                ui.horizontal(|ui| {
                                    let controls_width = 58.0;
                                    let response = ui.add_sized(
                                        [
                                            (ui.available_width() - controls_width).max(80.0),
                                            29.0,
                                        ],
                                        egui::TextEdit::singleline(
                                            &mut self.study_chat.rename_text,
                                        )
                                        .id(egui::Id::new("study-chat-inline-rename"))
                                        .margin(Margin::symmetric(9, 5)),
                                    );
                                    if self.study_chat.rename_focus_requested {
                                        response.request_focus();
                                        self.study_chat.rename_focus_requested = false;
                                    }
                                    commit = (response.has_focus()
                                        && ui.input(|input| input.key_pressed(Key::Enter)))
                                        || chat_icon_button(
                                            ui,
                                            ToolbarIcon::Confirm,
                                            normalized_chat_title(&self.study_chat.rename_text)
                                                .is_some(),
                                            "Save chat name (Enter)",
                                        )
                                        .clicked();
                                    cancel = (response.has_focus()
                                        && ui.input(|input| input.key_pressed(Key::Escape)))
                                        || chat_icon_button(
                                            ui,
                                            ToolbarIcon::Close,
                                            true,
                                            "Cancel rename (Escape)",
                                        )
                                        .clicked();
                                });
                                if cancel {
                                    self.cancel_study_chat_rename();
                                } else if commit {
                                    self.commit_study_chat_rename();
                                }
                            } else {
                                let active_title = self.study_chat.active_session().title.clone();
                                let history_width = ui.available_width().max(1.0);
                                let mut switch_to = None;
                                let history = chat_history_button(
                                    ui,
                                    &active_title,
                                    history_width,
                                    "Switch conversation",
                                    chat_font_size,
                                );
                                egui::Popup::menu(&history).show(|ui| {
                                    ui.set_min_width((history_width - 14.0).max(120.0));
                                    ui.set_max_width((history_width - 14.0).max(120.0));
                                    for chat in &self.study_chat.sessions {
                                        if ui
                                            .add_sized(
                                                [ui.available_width(), 30.0],
                                                egui::Button::selectable(
                                                    chat.id
                                                        == self.study_chat.active_session_id,
                                                    &chat.title,
                                                )
                                                .truncate(),
                                            )
                                            .clicked()
                                        {
                                            switch_to = Some(chat.id);
                                        }
                                    }
                                });
                                if let Some(id) = switch_to {
                                    self.activate_study_chat(id);
                                }
                            }
                        });
                    let divider = ui.min_rect();
                    ui.painter().hline(
                        divider.x_range(),
                        divider.bottom(),
                        Stroke::new(1.0, CHAT_DIVIDER),
                    );

                    let connected = self
                        .study_chat
                        .connected
                        .get(&provider)
                        .copied()
                        .unwrap_or(false);
                    let composer_content_rows = chat_prompt_visual_rows(
                        &self.study_chat.prompt,
                        (ui.available_width() - 46.0).max(1.0),
                    );
                    let composer_rows = chat_prompt_rows(
                        &self.study_chat.prompt,
                        (ui.available_width() - 46.0).max(1.0),
                    );
                    let footer_height = 104.0
                        + (composer_rows.saturating_sub(1) as f32 * 18.0)
                        + if self.study_chat.attachments.is_empty() { 0.0 } else { 32.0 };
                    let messages = self
                        .study_chat
                        .active_session()
                        .messages
                        .get(&provider)
                        .cloned()
                        .unwrap_or_default();
                    let latest_prompt = messages.iter().rposition(|message| message.role == ChatRole::User);
                    let status = self.study_chat.status.clone();
                    let busy = self.study_chat.busy;
                    let active_provider = self.study_chat.active_provider;
                    let mut copied_notice = None;

                    ScrollArea::vertical()
                        .id_salt("study-chat-messages")
                        .stick_to_bottom(true)
                        .auto_shrink([false, false])
                        .max_height((ui.available_height() - footer_height).max(80.0))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.add_space(16.0);
                            if messages.is_empty() {
                                chat_empty_state(ui, chat_font_size);
                                if chat_status_visible(&status, busy) {
                                    ui.add_space(12.0);
                                    chat_status(ui, &status, busy, chat_font_size);
                                }
                            }
                            for (index, message) in messages.iter().enumerate() {
                                let streaming = message.role == ChatRole::Assistant
                                    && message.text.is_empty()
                                    && index + 1 == messages.len()
                                    && busy
                                    && active_provider == Some(provider);
                                if !streaming
                                    && chat_message(
                                        ui,
                                        message,
                                        false,
                                        self.state.chat_font_size,
                                    )
                                {
                                    copied_notice = Some(if message.role == ChatRole::User {
                                        "Prompt copied."
                                    } else {
                                        "Reply copied."
                                    });
                                }
                                if latest_prompt == Some(index)
                                    && chat_status_visible(&status, busy)
                                {
                                    ui.add_space(3.0);
                                    chat_status(ui, &status, busy, chat_font_size);
                                }
                                ui.add_space(14.0);
                            }
                            ui.add_space(6.0);
                        });
                    if let Some(notice) = copied_notice {
                        self.study_chat.status = notice.to_owned();
                    }

                    Frame::new()
                        .fill(CHAT_CANVAS)
                        .inner_margin(Margin::symmetric(16, 12))
                        .show(ui, |ui| {
                            let mut remove = None;
                            if !self.study_chat.attachments.is_empty() {
                                ScrollArea::horizontal()
                                    .id_salt("study-chat-attachments")
                                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            for (index, attachment) in self.study_chat.attachments.iter().enumerate() {
                                                Frame::new()
                                                    .fill(CHAT_FIELD)
                                                    .stroke(Stroke::new(1.0, Color32::from_rgb(64, 64, 64)))
                                                    .corner_radius(CornerRadius::same(10))
                                                    .inner_margin(Margin::symmetric(8, 5))
                                                    .show(ui, |ui| {
                                                        ui.horizontal(|ui| {
                                                            ui.label(
                                                                RichText::new(format!(
                                                                    "{}  {}",
                                                                    attachment.kind_label(),
                                                                    elide(&attachment.name, 20)
                                                                ))
                                                                .size(chat_meta_text_size(chat_font_size))
                                                                .color(Color32::from_rgb(218, 218, 218)),
                                                            );
                                                            if ui.add(egui::Button::new("×").frame(false)).clicked() {
                                                                remove = Some(index);
                                                            }
                                                        });
                                                    });
                                            }
                                        });
                                    });
                                ui.add_space(6.0);
                            }
                            if let Some(index) = remove {
                                self.study_chat.attachments.remove(index);
                            }

                            let mut send_with_enter = false;
                            Frame::new()
                                .fill(CHAT_FIELD)
                                .stroke(Stroke::new(1.0, Color32::from_rgb(64, 64, 64)))
                                .corner_radius(CornerRadius::same(22))
                                .inner_margin(Margin::symmetric(12, 9))
                                .show(ui, |ui| {
                                    let input_height = chat_prompt_viewport_height(composer_rows);
                                    let content_height =
                                        chat_prompt_content_height(composer_content_rows);
                                    let input_width = ui.available_width().max(1.0);
                                    let prompt_response = ScrollArea::vertical()
                                        .id_salt("study-chat-prompt-scroll")
                                        .auto_shrink([false, false])
                                        .stick_to_bottom(true)
                                        .max_height(input_height)
                                        .min_scrolled_height(input_height)
                                        .scroll_bar_visibility(
                                            egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                                        )
                                        .show(ui, |ui| {
                                            ui.add_sized(
                                                [ui.available_width().max(1.0), content_height],
                                                egui::TextEdit::multiline(
                                                    &mut self.study_chat.prompt,
                                                )
                                                .id(egui::Id::new("study-chat-prompt"))
                                                .hint_text("Ask about this PDF or your files…")
                                                .desired_width(input_width)
                                                .desired_rows(composer_content_rows)
                                                .frame(Frame::NONE)
                                                .margin(Margin::ZERO),
                                            )
                                        })
                                        .inner;
                                    send_with_enter = prompt_response.has_focus()
                                        && ui.input(|input| {
                                            input.key_pressed(Key::Enter) && !input.modifiers.shift
                                        });
                                    ui.add_space(3.0);
                                    ui.horizontal(|ui| {
                                        let attach = chat_icon_button(
                                            ui,
                                            ToolbarIcon::Attach,
                                            true,
                                            "Attach a file or the current PDF",
                                        );
                                        egui::Popup::menu(&attach).show(|ui| {
                                            if ui.button("Choose PDF, image, or text…").clicked() {
                                                ui.close();
                                                self.choose_chat_attachment();
                                            }
                                            if ui
                                                .add_enabled(
                                                    self.current_path.is_some(),
                                                    egui::Button::new("Attach current PDF"),
                                                )
                                                .clicked()
                                                && let Some(path) = self.current_path.clone()
                                            {
                                                ui.close();
                                                self.add_chat_attachment(path);
                                            }
                                        });
                                        if chat_icon_button(
                                            ui,
                                            ToolbarIcon::Capture,
                                            self.current_path.is_some(),
                                            "Capture a PDF region for chat",
                                        )
                                        .clicked()
                                            && self.current_path.is_some()
                                        {
                                            self.tool_mode = ToolMode::ChatCapture;
                                            self.study_chat.status =
                                                "Drag over a figure or equation in the PDF.".to_owned();
                                        }
                                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                            if chat_send_button(ui, !self.study_chat.busy).clicked()
                                                && !self.study_chat.busy
                                            {
                                                self.send_chat_message();
                                            }
                                            ui.add_space(4.0);
                                            self.study_chat_compact_controls(
                                                ui,
                                                provider,
                                                connected,
                                                chat_font_size,
                                            );
                                        });
                                    });
                                });

                            if send_with_enter {
                                while self.study_chat.prompt.ends_with('\n') {
                                    self.study_chat.prompt.pop();
                                }
                                self.send_chat_message();
                            }

                        });
                });
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
                let (pointer_position, egui_pinch_zoom, ctrl_wheel_zoom, time) = ctx.input(|input| {
                    (
                        input.pointer.hover_pos().or_else(|| input.pointer.interact_pos()),
                        trackpad_pinch_factor(&input.events),
                        ctrl_mouse_wheel_zoom_factor(&input.events),
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
                let gesture_zoom = if use_native {
                    native_factor
                } else {
                    egui_pinch_zoom * ctrl_wheel_zoom
                };
                let over_viewer = pointer_position.is_none_or(|pos| viewer_rect.contains(pos));
                if over_viewer && (gesture_zoom - 1.0).abs() > 0.0001 {
                    // Eguis ScrollArea also consumes scroll deltas. A gesture
                    // must never both scale the page and scroll the document.
                    self.apply_pinch_zoom(
                        gesture_zoom,
                        Some(pointer_position.unwrap_or(viewer_rect.center())),
                        &ctx,
                    );
                }
                let pinch_in_progress = over_viewer
                    && (use_native
                        || (egui_pinch_zoom - 1.0).abs() > 0.0001
                        || (ctrl_wheel_zoom - 1.0).abs() > 0.0001);
                if pinch_in_progress {
                    // Native touchpads can deliver a pinch and a synthetic wheel
                    // event on separate paths. Discard the smooth-wheel movement;
                    // the document ScrollArea also has its wheel source disabled
                    // for this frame below.
                    ctx.input_mut(|input| input.smooth_scroll_delta = egui::Vec2::ZERO);
                }
                if pinch_in_progress && std::env::var_os("SNAPDF_PINCH_DEBUG").is_some() {
                    eprintln!(
                        "[snapdf zoom] source={} native={:.5} egui={:.5} wheel={:.5} zoom={:.4} old_offset={:?}",
                        if use_native { "xinput" } else { "egui" },
                        native_factor,
                        egui_pinch_zoom,
                        ctrl_wheel_zoom,
                        self.zoom,
                        self.document_scroll_offset,
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
                let mut presentation_fit_applied = false;
                if self.presentation_fit_height_requested {
                    if self.presentation_fit_delay_frames > 0 {
                        self.presentation_fit_delay_frames -= 1;
                        ctx.request_repaint();
                    } else if let Some(size) = self.page_sizes.get(self.current_page) {
                        // Keep every page completely visible. Tall pages are constrained by
                        // height; wide pages are constrained by width.
                        self.zoom = presentation_fit_zoom(
                            *size,
                            vec2(ui.available_width(), ui.available_height()),
                        );
                        self.document_scroll_offset = egui::Vec2::ZERO;
                        presentation_fit_applied = true;
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
                let canvas_width = if self.presentation_mode {
                    page_sizes
                        .get(presentation_page)
                        .map(|size| (size[0] * zoom).max(ui.available_width()))
                        .unwrap_or_else(|| ui.available_width())
                } else {
                    page_sizes
                        .iter()
                        .map(|size| size[0] * zoom + 56.0)
                        .fold(ui.available_width(), f32::max)
                };

                // Solve the new scroll offset *before* egui lays out the pages.
                // A queued scroll_with_delta() adjusts a later frame and causes
                // oscillation when gesture updates arrive every frame.
                let anchored_offset = self.pinch_anchor.take().map(|anchor| {
                    let previous_canvas_width = if self.presentation_mode {
                        page_sizes
                            .get(presentation_page)
                            .map(|size| {
                                (size[0] * zoom_before_pinch).max(ui.available_width())
                            })
                            .unwrap_or_else(|| ui.available_width())
                    } else {
                        page_sizes
                            .iter()
                            .map(|size| size[0] * zoom_before_pinch + 56.0)
                            .fold(ui.available_width(), f32::max)
                    };
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
                } else if presentation_fit_applied {
                    document_scroll = document_scroll.scroll_offset(egui::Vec2::ZERO);
                }
                let scroll_output = document_scroll
                    .show_viewport(ui, |ui, viewport| {
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
                            let row_height = if self.presentation_mode {
                                viewport.height().max(display_size.y)
                            } else {
                                display_size.y + page_gap
                            };
                            let (row_rect, _) = ui.allocate_exact_size(
                                vec2(row_width, row_height),
                                Sense::hover(),
                            );
                            let page_rect = if self.presentation_mode {
                                centered_page_rect(row_rect, display_size)
                            } else {
                                Rect::from_min_size(
                                    pos2(
                                        row_rect.center().x - display_size.x / 2.0,
                                        row_rect.min.y,
                                    ),
                                    display_size,
                                )
                            };
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
                                // The worker only renders near the focused page;
                                // asking for farther pages would just bounce back
                                // as skipped requests every frame.
                                if needs_render && index.abs_diff(visible_page) <= 2 {
                                    // A blank page gets a quick low-resolution
                                    // pass first; the sharp render replaces it.
                                    if !self.textures.get(&index).is_some_and(|page| {
                                        page.generation == generation
                                    }) && target_width > PREVIEW_RENDER_WIDTH
                                    {
                                        self.request_page(PageKey {
                                            width: PREVIEW_RENDER_WIDTH,
                                            ..key
                                        });
                                    }
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
                                            if response.drag_started() || response.clicked() {
                                                    // A click anywhere on the page begins a new
                                                    // selection gesture. Clear first so a click in
                                                    // whitespace cannot leave the nearest old
                                                    // character highlighted.
                                                    self.text_selection = None;
                                                    self.text_selection_dragging = false;
                                                    self.pending_select_all_text = false;
                                                    self.pending_selection_copy = false;
                                                    self.pending_selection_highlight = false;
                                                    ctx.plugin::<egui::text_selection::LabelSelectionState>()
                                                        .lock()
                                                        .clear_selection();
                                                    if let Some(glyphs) = self.text_layers.get(&index)
                                                        && let Some(pointer) = response.interact_pointer_pos()
                                                        && let Some(glyph) = nearest_glyph(glyphs, page_rect, source_size, pointer)
                                                    {
                                                        ctx.memory_mut(|memory| memory.stop_text_input());
                                                        let position = TextPosition { page: index, glyph };
                                                        self.text_selection = Some(TextSelection { anchor: position, focus: position });
                                                        self.text_selection_dragging = response.drag_started();
                                                        self.pending_select_all_text = false;
                                                        self.pending_selection_copy = false;
                                                        self.pending_selection_highlight = false;
                                                    }
                                            } else if response.dragged()
                                                    && self.text_selection_dragging
                                                    && let Some(glyphs) = self.text_layers.get(&index)
                                                    && let Some(pointer) = response.interact_pointer_pos()
                                                    && page_rect.contains(pointer)
                                                    && let Some(glyph) = nearest_drag_glyph(glyphs, page_rect, source_size, pointer)
                                                    && let Some(selection) = self.text_selection.as_mut()
                                                {
                                                    selection.focus = TextPosition { page: index, glyph };
                                                }
                                            if response.hovered() { ctx.set_cursor_icon(CursorIcon::Text); }
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
                                        ToolMode::Ocr | ToolMode::MathOcr | ToolMode::ChatCapture => {
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
                                                    if self.tool_mode == ToolMode::Ocr && !embedded.trim().is_empty() {
                                                        self.ocr_popup = Some(OcrPopup { text: embedded, detail: "Exact PDF text layer - instant - no OCR needed".to_owned(), busy: false });
                                                    } else {
                                                        let request_id = self.next_ocr_request;
                                                        self.next_ocr_request = self.next_ocr_request.wrapping_add(1);
                                                        let math = self.tool_mode == ToolMode::MathOcr;
                                                        let chat_capture = self.tool_mode == ToolMode::ChatCapture;
                                                        if math { self.math_ocr_requests.insert(request_id); }
                                                        if chat_capture {
                                                            self.chat_capture_requests.insert(request_id, index);
                                                            self.study_chat.status = "Preparing PDF image…".to_owned();
                                                        } else {
                                                            self.ocr_popup = Some(OcrPopup {
                                                                text: String::new(),
                                                                detail: if math {
                                                                    "Preparing formula crop; first use may finish the one-time model download...".to_owned()
                                                                } else {
                                                                    "Preparing high-resolution crop...".to_owned()
                                                                },
                                                                busy: true,
                                                            });
                                                        }
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
                            && let Some(action) = selection_action_bar(
                                &ctx,
                                rect,
                                !self.state.hide_study_chat,
                            )
                        {
                            match action {
                                SelectionAction::Copy => {
                                    self.copy_text_selection(&ctx);
                                }
                                SelectionAction::Highlight => self.highlight_text_selection(),
                                SelectionAction::Chat => {
                                    if let Some(text) = self.selected_text() {
                                        self.study_chat.attachments.push(ChatAttachment::text(
                                            "Selected PDF text",
                                            text,
                                        ));
                                        self.study_chat.visible = true;
                                        self.study_chat.status =
                                            "Selected text attached. Add your question and send."
                                                .to_owned();
                                    }
                                }
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

    fn copy_text_selection(&mut self, ctx: &egui::Context) {
        if self.text_selection.is_none() {
            return;
        }
        if self.selection_text_ready() {
            if let Some(text) = self.selected_text() {
                ctx.copy_text(text);
                self.pending_selection_copy = false;
                self.status = "Selected text copied.".to_owned();
            }
            return;
        }

        self.pending_selection_copy = true;
        if let Some(selection) = self.text_selection {
            let (first, last) = selection.ordered();
            for page in first.page..=last.page {
                self.request_text(page, self.generation);
            }
        }
        self.status = "Finishing text extraction before copying…".to_owned();
    }

    fn finish_pending_selection_copy(&mut self, ctx: &egui::Context) {
        if self.pending_selection_copy && self.selection_text_ready() {
            self.copy_text_selection(ctx);
        }
    }

    fn select_all_document_text(&mut self) {
        if self.page_sizes.is_empty() {
            return;
        }
        self.pending_select_all_text = true;
        self.pending_selection_copy = false;
        self.text_selection_dragging = false;
        self.pending_selection_highlight = false;
        for page in 0..self.page_sizes.len() {
            self.request_text(page, self.generation);
        }
        self.status = "Selecting all text in this PDF…".to_owned();
        self.finish_select_all_text();
    }

    fn finish_select_all_text(&mut self) {
        if !self.pending_select_all_text
            || (0..self.page_sizes.len()).any(|page| !self.text_layers.contains_key(&page))
        {
            return;
        }

        self.pending_select_all_text = false;
        if let Some(selection) =
            full_document_text_selection(&self.text_layers, self.page_sizes.len())
        {
            self.text_selection = Some(selection);
            self.status = "Selected all PDF text. Press Ctrl+C to copy.".to_owned();
        } else {
            self.text_selection = None;
            self.status = "This PDF has no extractable text.".to_owned();
        }
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
        let Some(rect) = self
            .rename_anchor
            .or_else(|| ctx.data(|data| data.get_temp::<Rect>(rename_row_id(&target))))
        else {
            return;
        };
        let mut save = false;
        let mut cancel = false;
        egui::Area::new(egui::Id::new("sidebar-inline-rename"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .show(ctx, |ui| {
                let response = ui.add_sized(
                    rect.size(),
                    egui::TextEdit::singleline(&mut self.rename_buffer)
                        .hint_text("Display name")
                        .margin(Margin::symmetric(5, 2)),
                );
                if self.rename_focus_requested {
                    response.request_focus();
                    self.rename_focus_requested = false;
                }
                let enter = ui.input(|input| input.key_pressed(Key::Enter));
                cancel = ui.input(|input| input.key_pressed(Key::Escape));
                save = !cancel
                    && !self.rename_buffer.trim().is_empty()
                    && (enter || response.lost_focus());
            });

        if cancel {
            self.rename_target = None;
            self.rename_anchor = None;
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
            self.rename_anchor = None;
            self.persist();
        }
    }

    fn note_dialog(&mut self, ctx: &egui::Context) {
        let Some(draft) = self.note_draft.as_mut() else {
            return;
        };
        let editing = draft.annotation_id.is_some();
        let mut save = false;
        let close = popover_dialog(
            ctx,
            "note-dialog",
            if editing {
                "Edit comment"
            } else {
                "Add comment"
            },
            draft.screen_anchor.map(|anchor| anchor + vec2(18.0, -24.0)),
            320.0,
            |ui| {
                let editor = ui.add_sized(
                    [ui.available_width(), 84.0],
                    egui::TextEdit::multiline(&mut draft.text)
                        .hint_text("Write a comment")
                        .desired_rows(3)
                        .margin(Margin::symmetric(10, 8)),
                );
                let enter_to_save = editor.has_focus()
                    && ui.input(|input| input.key_pressed(Key::Enter) && !input.modifiers.shift);
                if enter_to_save && !draft.text.trim().is_empty() {
                    save = true;
                }
                ui.horizontal(|ui| {
                    dialog_hint(ui, "Shift+Enter for a new line");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        save |= primary_button(
                            ui,
                            if editing { "Save" } else { "Add" },
                            !draft.text.trim().is_empty(),
                        )
                        .clicked();
                    });
                });
            },
        );
        if close {
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
        // While the Overall slider is being dragged its value is only previewed:
        // applying it would rescale this panel under the pointer.
        let mut interface_scale = self.pending_interface_scale.unwrap_or(self.state.ui_scale);
        let mut overall_dragging = false;
        let mut toolbar_ratio = self.state.chrome_scale_ratio;
        let mut sidebar_ratio = self.state.sidebar_scale_ratio;
        let mut chat_font_size = self.state.chat_font_size;
        let mut file_icons = self.state.sidebar_file_icons;
        let mut path_bar = self.state.path_bar;
        let mut changed = false;
        let mut set_default_pdf_viewer = false;
        let mut close = false;

        let screen = ctx.content_rect();
        let size = vec2(
            (screen.width() - 48.0).clamp(320.0, 720.0),
            (screen.height() - 48.0).clamp(300.0, 560.0),
        );
        const NAV_WIDTH: f32 = 184.0;
        let modal = egui::Modal::new(egui::Id::new("settings-modal"))
            .backdrop_color(Color32::from_black_alpha(130))
            .frame(
                Frame::new()
                    .fill(SETTINGS_BG)
                    .stroke(Stroke::new(1.0, Color32::from_rgb(58, 58, 58)))
                    .corner_radius(CornerRadius::same(12))
                    .shadow(egui::Shadow {
                        offset: [0, 10],
                        blur: 32,
                        spread: 0,
                        color: Color32::from_black_alpha(140),
                    })
                    .inner_margin(Margin::ZERO),
            )
            .show(ctx, |ui| {
                let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
                let nav_rect = Rect::from_min_size(rect.min, vec2(NAV_WIDTH, size.y));
                let content_rect =
                    Rect::from_min_max(pos2(rect.left() + NAV_WIDTH, rect.top()), rect.max);
                ui.painter().rect_filled(
                    nav_rect,
                    CornerRadius {
                        nw: 12,
                        ne: 0,
                        sw: 12,
                        se: 0,
                    },
                    SETTINGS_NAV_BG,
                );
                // Category navigation.
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(nav_rect.shrink2(vec2(10.0, 18.0)))
                        .layout(Layout::top_down(Align::Min)),
                    |ui| {
                        ui.horizontal(|ui| {
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new("Settings")
                                    .size(19.0)
                                    .strong()
                                    .color(Color32::from_rgb(240, 240, 240)),
                            );
                        });
                        ui.add_space(14.0);
                        for page in [
                            SettingsPage::General,
                            SettingsPage::Appearance,
                            SettingsPage::StudyChat,
                        ] {
                            if settings_nav_item(ui, page.label(), self.settings_page == page)
                                .clicked()
                            {
                                self.settings_page = page;
                            }
                            ui.add_space(2.0);
                        }
                    },
                );
                // Page content.
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(Rect::from_min_max(
                            content_rect.min + vec2(24.0, 14.0),
                            content_rect.max - vec2(10.0, 12.0),
                        ))
                        .layout(Layout::top_down(Align::Min)),
                    |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(self.settings_page.label())
                                    .size(17.0)
                                    .strong()
                                    .color(Color32::from_rgb(240, 240, 240)),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if chrome_button(ui, ToolbarIcon::Close, false, true, "Close (Esc)")
                                    .clicked()
                                {
                                    close = true;
                                }
                            });
                        });
                        ui.add_space(8.0);
                        ScrollArea::vertical()
                            .id_salt("settings-scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width() - 14.0);
                                ui.spacing_mut().item_spacing.y = 10.0;
                                ui.spacing_mut().slider_width = 170.0;
                    match self.settings_page {
                        SettingsPage::General => {
                            self.settings_updates_card(ui);
                            settings_card(ui, |ui| {
                                let current =
                                    self.pdf_default_application.clone();
                                let is_default =
                                    current.as_deref().is_some_and(|id| {
                                        id == "io.snapdf.SnaPdf.desktop"
                                    });
                                let description = if is_default {
                                    "SnaPdf opens PDFs from your file manager and browser.".to_owned()
                                } else if let Some(id) = current {
                                    format!("PDFs currently open in {}.", id.trim_end_matches(".desktop"))
                                } else {
                                    "Installing SnaPdf never changes your default.".to_owned()
                                };
                                settings_row(
                                    ui,
                                    "Default PDF app",
                                    &description,
                                    |ui| {
                                        if is_default {
                                            ui.label(
                                                RichText::new("✓ Default")
                                                    .color(SETTINGS_OK),
                                            );
                                        } else if ui
                                            .button("Make default")
                                            .clicked()
                                        {
                                            set_default_pdf_viewer = true;
                                        }
                                    },
                                );
                            });
                        }
                        SettingsPage::Appearance => {
                            settings_card(ui, |ui| {
                                settings_card_title(
                                    ui,
                                    "Interface size",
                                    "PDF zoom is separate: use the toolbar, pinch, or Ctrl+scroll.",
                                );
                                for (label, value, default) in [
                                    (
                                        "Overall",
                                        &mut interface_scale,
                                        DEFAULT_UI_SCALE,
                                    ),
                                    (
                                        "Top bar",
                                        &mut toolbar_ratio,
                                        DEFAULT_CHROME_SCALE_RATIO,
                                    ),
                                    (
                                        "File explorer",
                                        &mut sidebar_ratio,
                                        DEFAULT_SIDEBAR_SCALE_RATIO,
                                    ),
                                ] {
                                    settings_divider(ui);
                                    settings_row(ui, label, "", |ui| {
                                        let response = settings_slider(
                                            ui,
                                            value,
                                            default * 0.5..=default * 2.0,
                                            (default * 0.05) as f64,
                                            |value| {
                                                format!(
                                                    "{}%",
                                                    normalized_scale_percent(value as f64, default)
                                                )
                                            },
                                        );
                                        changed |= response.changed();
                                        overall_dragging |=
                                            label == "Overall" && response.dragged();
                                    });
                                }
                                settings_divider(ui);
                                ui.horizontal(|ui| {
                                    ui.with_layout(
                                        Layout::right_to_left(Align::Center),
                                        |ui| {
                                            if ui
                                                .button("Reset to 100%")
                                                .clicked()
                                            {
                                                interface_scale =
                                                    DEFAULT_UI_SCALE;
                                                toolbar_ratio =
                                                    DEFAULT_CHROME_SCALE_RATIO;
                                                sidebar_ratio =
                                                    DEFAULT_SIDEBAR_SCALE_RATIO;
                                                changed = true;
                                            }
                                        },
                                    );
                                });
                            });
                            settings_card(ui, |ui| {
                                settings_row(
                                    ui,
                                    "File path",
                                    "A full path bar under the tabs, or a compact folder button in the toolbar.",
                                    |ui| {
                                        changed |= settings_segmented(
                                            ui,
                                            &mut path_bar,
                                            &[
                                                (PathBarStyle::Bar, "Path bar"),
                                                (PathBarStyle::InToolbar, "In toolbar"),
                                            ],
                                        );
                                    },
                                );
                                settings_divider(ui);
                                settings_row(
                                    ui,
                                    "PDF icons in explorer",
                                    "How PDFs are marked in the project tree.",
                                    |ui| {
                                        changed |= settings_segmented(
                                            ui,
                                            &mut file_icons,
                                            &[
                                                (SidebarFileIcons::Standard, "Standard"),
                                                (SidebarFileIcons::VsCode, "VS Code"),
                                                (SidebarFileIcons::None, "None"),
                                            ],
                                        );
                                    },
                                );
                            });
                        }
                        SettingsPage::StudyChat => {
                            settings_card(ui, |ui| {
                                let mut enabled = !self.state.hide_study_chat;
                                settings_row(
                                    ui,
                                    "Study chat",
                                    "Show the chat panel and its actions. Ctrl+Shift+A toggles the panel.",
                                    |ui| {
                                        if toggle_switch(ui, &mut enabled)
                                            .changed()
                                        {
                                            self.state.hide_study_chat =
                                                !enabled;
                                            changed = true;
                                        }
                                    },
                                );
                                settings_divider(ui);
                                settings_row(
                                    ui,
                                    "Text size",
                                    "Message text in the chat panel.",
                                    |ui| {
                                        changed |= settings_slider(
                                            ui,
                                            &mut chat_font_size,
                                            11.0..=28.0,
                                            0.5,
                                            |value| format!("{value:.1} pt"),
                                        )
                                        .changed();
                                    },
                                );
                                settings_divider(ui);
                                settings_row(
                                    ui,
                                    "Confirm before replacing chats",
                                    "Up to five chats are kept; a sixth replaces the oldest.",
                                    |ui| {
                                        changed |= toggle_switch(
                                            ui,
                                            &mut self
                                                .state
                                                .warn_before_replacing_study_chat,
                                        )
                                        .changed();
                                    },
                                );
                            });
                        }
                    }
                                ui.add_space(12.0);
                            });
                    },
                );
            });
        close |= modal.should_close();
        if set_default_pdf_viewer {
            self.set_default_pdf_viewer();
        }
        if overall_dragging {
            self.pending_interface_scale = Some(interface_scale);
            interface_scale = self.state.ui_scale;
            changed = false;
        } else if self.pending_interface_scale.take().is_some() {
            // The drag just ended: apply the previewed scale once.
            changed = true;
        }
        if close {
            self.show_view_settings = false;
        }
        if changed {
            let previous_interface_zoom = interface_zoom(self.state.ui_scale);
            let next_interface_zoom = interface_zoom(interface_scale);
            if (next_interface_zoom - previous_interface_zoom).abs() > f32::EPSILON {
                // egui's zoom factor scales every painted point. Counter-scale
                // the document so changing interface size never changes the PDF.
                self.zoom =
                    (self.zoom * previous_interface_zoom / next_interface_zoom).clamp(0.2, 8.0);
            }
            self.state.ui_scale = interface_scale;
            self.state.chrome_scale_ratio = toolbar_ratio;
            self.state.sidebar_scale_ratio = sidebar_ratio;
            self.state.chat_font_size = chat_font_size;
            self.state.sidebar_file_icons = file_icons;
            self.state.path_bar = path_bar;
            self.state.sidebar_file_icons_initialized = true;
            if self.state.hide_study_chat {
                self.study_chat.visible = false;
                self.study_chat.show_gemini_key = false;
                if self.tool_mode == ToolMode::ChatCapture {
                    self.tool_mode = ToolMode::Select;
                    self.drag_start = None;
                }
            }
            ctx.set_zoom_factor(next_interface_zoom);
            self.persist();
        }
    }

    fn set_default_pdf_viewer(&mut self) {
        match Command::new("xdg-mime")
            .args(["default", "io.snapdf.SnaPdf.desktop", "application/pdf"])
            .status()
        {
            Ok(status) if status.success() => {
                self.pdf_default_application = Some("io.snapdf.SnaPdf.desktop".to_owned());
                self.status = "SnaPdf is now the default PDF application.".to_owned();
            }
            Ok(status) => {
                self.status = format!(
                    "Could not set the default PDF application (xdg-mime exited with {status})."
                );
            }
            Err(error) => {
                self.status = format!("Could not run xdg-mime: {error}");
            }
        }
    }

    fn study_chat_replacement_dialog(&mut self, ctx: &egui::Context) {
        if !self.study_chat.pending_replacement_warning {
            return;
        }
        let mut replace = false;
        let mut cancel = false;
        let mut do_not_show_again = false;
        let close = modal_dialog(
            ctx,
            "replace-chat",
            "Replace oldest chat?",
            380.0,
            true,
            |ui| {
                dialog_text(
                    ui,
                    "Study chat keeps up to five conversations. Starting another replaces the one you opened least recently.",
                );
                ui.checkbox(&mut do_not_show_again, "Don't ask again");
                dialog_actions(ui, |ui| {
                    replace = primary_button(ui, "Replace oldest", true).clicked();
                    cancel = secondary_button(ui, "Cancel").clicked();
                });
            },
        );
        if close || cancel {
            self.study_chat.pending_replacement_warning = false;
        }
        if replace {
            if do_not_show_again {
                self.state.warn_before_replacing_study_chat = false;
            }
            self.replace_oldest_study_chat();
        }
    }

    fn save_to_source_dialog(&mut self, ctx: &egui::Context) {
        if !self.confirm_save_to_source {
            return;
        }
        let mut save = false;
        let mut cancel = false;
        let close = modal_dialog(
            ctx,
            "save-to-source",
            "Save annotations into this PDF?",
            400.0,
            true,
            |ui| {
                dialog_text(
                    ui,
                    "Your annotations are written into the file on disk, so other PDF viewers show them too.",
                );
                dialog_hint(
                    ui,
                    "A timestamped backup of the original is kept in the hidden .snapdf-backups folder.",
                );
                dialog_actions(ui, |ui| {
                    save = primary_button(ui, "Save to PDF", true).clicked();
                    cancel = secondary_button(ui, "Cancel").clicked();
                });
            },
        );
        if close || cancel {
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
        let mut add_to_chat = None;
        let hide_chat = self.state.hide_study_chat;
        let close = modal_dialog(ctx, "ocr-result", "Recognized text", 560.0, true, |ui| {
            ui.horizontal(|ui| {
                if popup.busy {
                    ui.spinner();
                }
                dialog_hint(ui, popup.detail.clone());
            });
            // A fixed scroll region keeps the actions in view even when OCR
            // returns several pages of text.
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), 240.0),
                Layout::top_down(Align::Min),
                |ui| {
                    ScrollArea::vertical()
                        .id_salt("ocr-result-text-scroll")
                        .show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut popup.text)
                                    .desired_width(ui.available_width())
                                    .desired_rows(12)
                                    .margin(Margin::symmetric(10, 8))
                                    .hint_text("Recognized text appears here"),
                            );
                        });
                },
            );
            dialog_actions(ui, |ui| {
                if primary_button(ui, "Copy text", !popup.text.is_empty()).clicked() {
                    ctx.copy_text(popup.text.clone());
                    popup.detail = "Copied to the clipboard.".to_owned();
                }
                if !hide_chat
                    && ui
                        .add_enabled_ui(!popup.text.is_empty(), |ui| {
                            secondary_button(ui, "Ask in study chat")
                        })
                        .inner
                        .clicked()
                {
                    add_to_chat = Some(popup.text.clone());
                }
            });
        });
        if let Some(text) = add_to_chat {
            self.study_chat
                .attachments
                .push(ChatAttachment::text("OCR text snip", text));
            self.study_chat.visible = true;
            self.study_chat.status =
                "OCR text attached. Nothing was copied to the clipboard.".to_owned();
            self.ocr_popup = None;
            return;
        }
        if close {
            self.ocr_popup = None;
        }
    }

    fn gemini_key_dialog(&mut self, ctx: &egui::Context) {
        if !self.study_chat.show_gemini_key {
            return;
        }
        let mut connect = false;
        let close = modal_dialog(ctx, "gemini-key", "Connect Gemini", 420.0, false, |ui| {
            dialog_text(ui, "Paste an API key from Google AI Studio.");
            let response = ui.add_sized(
                [ui.available_width(), 32.0],
                egui::TextEdit::singleline(&mut self.study_chat.gemini_key)
                    .password(true)
                    .hint_text("Gemini API key")
                    .vertical_align(Align::Center)
                    .margin(Margin::symmetric(10, 6)),
            );
            if response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter)) {
                connect = true;
            }
            dialog_hint(
                ui,
                "The key is stored only in SnaPdf's private credentials file and sent only to Google's Gemini API.",
            );
            dialog_actions(ui, |ui| {
                connect |=
                    primary_button(ui, "Connect", !self.study_chat.gemini_key.trim().is_empty())
                        .clicked();
                if secondary_button(ui, "Get a key").clicked() {
                    let _ = webbrowser::open("https://aistudio.google.com/app/apikey");
                }
            });
        });
        if connect && !self.study_chat.gemini_key.trim().is_empty() {
            let key = std::mem::take(&mut self.study_chat.gemini_key);
            self.study_chat.status = "Checking Gemini connection…".to_owned();
            let _ = self
                .chat_worker
                .commands
                .send(ChatCommand::ConnectGemini { api_key: key });
            self.study_chat.show_gemini_key = false;
        } else if close {
            self.study_chat.show_gemini_key = false;
        }
    }

    fn text_annotation_dialog(&mut self, ctx: &egui::Context) {
        let Some(draft) = self.text_draft.as_mut() else {
            return;
        };
        let mut save = false;
        let mut cancel = false;
        let stamp = draft.stamp;
        let close = modal_dialog(
            ctx,
            "text-annotation",
            if stamp { "Add stamp" } else { "Add text box" },
            400.0,
            false,
            |ui| {
                let editor = ui.add_sized(
                    [ui.available_width(), if stamp { 32.0 } else { 120.0 }],
                    if stamp {
                        egui::TextEdit::singleline(&mut draft.text)
                            .hint_text("Stamp text, for example APPROVED")
                            .vertical_align(Align::Center)
                            .margin(Margin::symmetric(10, 6))
                    } else {
                        egui::TextEdit::multiline(&mut draft.text)
                            .hint_text("Text to place on the page")
                            .margin(Margin::symmetric(10, 8))
                    },
                );
                if editor.lost_focus()
                    && stamp
                    && ui.input(|input| input.key_pressed(Key::Enter))
                    && !draft.text.trim().is_empty()
                {
                    save = true;
                }
                dialog_actions(ui, |ui| {
                    save |= primary_button(ui, "Add", !draft.text.trim().is_empty()).clicked();
                    cancel = secondary_button(ui, "Cancel").clicked();
                });
            },
        );
        if close || cancel {
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
        let close = modal_dialog(ctx, "open-with", "Open with", 400.0, true, |ui| {
            dialog_hint(ui, format!("Open {} in another PDF app.", file_name(&path)));
            ui.add_space(2.0);
            for app in &apps {
                let (rect, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
                if response.hovered() {
                    ui.painter().rect_filled(
                        rect,
                        CornerRadius::same(8),
                        Color32::from_rgb(48, 48, 48),
                    );
                }
                paint_vscode_pdf_icon(ui.painter(), rect.left_center() + vec2(18.0, 0.0), 1.1);
                ui.painter().text(
                    rect.left_center() + vec2(40.0, 0.0),
                    Align2::LEFT_CENTER,
                    &app.name,
                    FontId::proportional(14.0),
                    Color32::from_rgb(228, 228, 228),
                );
                if response.clicked() {
                    selected = Some(app.clone());
                }
            }
            if let Some(error) = &self.open_with_error {
                ui.label(
                    RichText::new(error)
                        .size(12.5)
                        .color(Color32::from_rgb(235, 120, 110)),
                );
            }
        });
        if close {
            self.open_with_path = None;
        } else if let Some(app) = selected {
            self.launch_external_app(&app, &path);
        }
    }

    fn print_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.print_dialog.as_mut() else {
            return;
        };
        let mut submit = false;
        let mut cancel = false;
        let close = modal_dialog(ctx, "print", "Print", 420.0, false, |ui| {
            dialog_field_label(ui, "Printer");
            if dialog.printers.is_empty() {
                dialog_hint(
                    ui,
                    "No printers were found. The job goes to the system default printer.",
                );
            } else {
                egui::ComboBox::from_id_salt("printer-picker")
                    .selected_text(if dialog.printer.is_empty() {
                        "System default"
                    } else {
                        &dialog.printer
                    })
                    .width(ui.available_width())
                    .height(240.0)
                    .show_ui(ui, |ui| {
                        for printer in &dialog.printers {
                            ui.selectable_value(&mut dialog.printer, printer.clone(), printer);
                        }
                    });
            }
            ui.add_space(4.0);
            dialog_field_label(ui, "Pages");
            ui.add_sized(
                [ui.available_width(), 32.0],
                egui::TextEdit::singleline(&mut dialog.page_range)
                    .hint_text("All pages, or a range like 1-4, 7")
                    .vertical_align(Align::Center)
                    .margin(Margin::symmetric(10, 6)),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                dialog_field_label(ui, "Copies");
                ui.add(egui::DragValue::new(&mut dialog.copies).range(1..=99));
            });
            dialog_hint(ui, "Annotations are printed with the page.");
            dialog_actions(ui, |ui| {
                submit = primary_button(ui, "Print", true).clicked();
                cancel = secondary_button(ui, "Cancel").clicked();
            });
        });
        if close || cancel {
            self.print_dialog = None;
        } else if submit {
            self.submit_print();
        }
    }

    fn empty_state(&mut self, ui: &mut egui::Ui) {
        let content_height = if self.renderer_ready { 181.0 } else { 209.0 };
        let content_rect = Rect::from_center_size(
            ui.available_rect_before_wrap().center(),
            vec2(238.0, content_height),
        );
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(content_rect)
                .layout(Layout::top_down(Align::Center)),
            |ui| {
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
                ui.add_space(7.0);
                if action_frame(ui, "Search", "Ctrl+F").clicked() {
                    self.open_search();
                }
                ui.add_space(7.0);
                if action_frame(ui, "Study chat", "Ctrl+Shift+A").clicked() {
                    if self.state.hide_study_chat {
                        self.status = "Study chat is disabled in View settings.".to_owned();
                    } else {
                        self.study_chat.visible = true;
                    }
                }
                if !self.renderer_ready {
                    ui.add_space(12.0);
                    ui.spinner();
                }
            },
        );
    }
}

impl eframe::App for SnaPdfApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame_number = self.frame_number.wrapping_add(1);
        self.receive_instance_handoffs(ctx);
        self.receive_update_events(ctx);
        self.handle_events(ctx);
        self.keyboard_shortcuts(ctx);
        self.dropped_files(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Top chrome first so the tab strip and toolbar span the full width,
        // with the sidebar sitting beneath them like Edge's contents pane.
        self.toolbar(ui);
        self.sidebar(ui);
        self.study_chat_panel(ui);
        self.viewer(ui);
        self.mode_controls(ui.ctx());
        self.rename_dialog(ui.ctx());
        self.note_dialog(ui.ctx());
        self.view_settings_dialog(ui.ctx());
        self.study_chat_replacement_dialog(ui.ctx());
        self.save_to_source_dialog(ui.ctx());
        self.text_annotation_dialog(ui.ctx());
        self.ocr_result_dialog(ui.ctx());
        self.gemini_key_dialog(ui.ctx());
        self.open_with_dialog(ui.ctx());
        self.print_dialog(ui.ctx());
        self.update_prompt_dialog(ui.ctx());
        self.password_dialog(ui.ctx());
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = self.worker.commands.send(RenderCommand::Shutdown);
        let _ = self.ocr_worker.commands.send(OcrCommand::Shutdown);
        let _ = self.chat_worker.commands.send(ChatCommand::Shutdown);
        if let Some(socket) = self.instance_socket.as_ref() {
            let _ = fs::remove_file(socket);
        }
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
}

fn rail_button(ui: &mut egui::Ui, selected: bool, icon: RailIcon) -> Response {
    let scale = sidebar_visual_scale(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(34.0, 30.0) * scale, Sense::click());
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
    }
    response
}

struct SidebarRowResponse {
    body: Response,
    options: Response,
    rename_rect: Rect,
    /// The label was elided, so callers should offer the full name on hover.
    truncated: bool,
}

fn rename_row_id(target: &RenameTarget) -> egui::Id {
    egui::Id::new(("sidebar-rename-row", target))
}

fn register_rename_row(ui: &egui::Ui, target: &RenameTarget, rect: Rect) {
    ui.ctx()
        .data_mut(|data| data.insert_temp(rename_row_id(target), rect));
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

/// Item counts and other sidebar metadata: quieter than names, still legible.
const SIDEBAR_COUNT: Color32 = Color32::from_rgb(172, 172, 172);

/// Left offset of a project-tree row; shallow steps keep deep files readable.
fn tree_indent(depth: usize) -> f32 {
    8.0 + depth as f32 * 12.0
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
            tree_indent(depth),
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
            &row.body,
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
            // Line file names up with the names of sibling folders, which sit
            // after a chevron and folder icon.
            tree_indent(depth)
                + if file_icons == SidebarFileIcons::None {
                    24.0
                } else {
                    6.0
                },
            file_icons,
        );
        register_rename_row(ui, &RenameTarget::File(path.clone()), row.rename_rect);
        if row.truncated {
            row.body.clone().on_hover_text(label.clone());
        }
        if row.body.clicked() && !row.options.clicked() {
            action = Some(TreeAction::Open(path.clone()));
        }
        if let Some(menu_action) = sidebar_options_popup(
            &row.options,
            &row.body,
            aliases.contains_key(path),
            None,
            true,
            pinned_files.contains(path),
        ) {
            action = match menu_action {
                SidebarMenuAction::OpenWith => Some(TreeAction::OpenWith(path.clone())),
                SidebarMenuAction::OpenInFiles => Some(TreeAction::OpenInFiles(path.clone())),
                SidebarMenuAction::Rename => {
                    Some(TreeAction::Rename(path.clone(), label, row.rename_rect))
                }
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

fn toolbar_visual_scale(ui: &egui::Ui) -> f32 {
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
    let count_rect = ui.painter().text(
        pos2(rect.right() - 7.0 * scale, y),
        Align2::RIGHT_CENTER,
        count.to_string(),
        FontId::proportional(11.5 * scale),
        SIDEBAR_COUNT,
    );
    let label_right = if response.hovered() {
        rect.right() - 34.0 * scale
    } else {
        count_rect.left() - 8.0 * scale
    };
    let truncated = paint_elided_label(
        ui.painter(),
        pos2(x + 32.0 * scale, y),
        name,
        label_right - (x + 32.0 * scale),
        FontId::proportional(13.0 * scale),
        Color32::from_rgb(232, 232, 232),
    );
    let options = sidebar_options_button(ui, rect, &response);
    SidebarRowResponse {
        body: response,
        options,
        truncated,
        rename_rect: Rect::from_min_max(
            pos2(x + 31.0 * scale, rect.top() + 2.0 * scale),
            pos2(rect.right() - 35.0 * scale, rect.bottom() - 2.0 * scale),
        ),
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

    let count_rect = painter.text(
        pos2(rect.right() - 34.0 * scale, center_y),
        Align2::RIGHT_CENTER,
        count.to_string(),
        FontId::proportional(12.0 * scale),
        SIDEBAR_COUNT,
    );
    let text_left = rect.left() + 51.0 * scale;
    let truncated = paint_elided_label(
        painter,
        pos2(text_left, center_y),
        name,
        count_rect.left() - 8.0 * scale - text_left,
        FontId::proportional(14.0 * scale),
        Color32::from_rgb(218, 220, 225),
    );
    let options = sidebar_options_button(ui, rect, &response);
    SidebarRowResponse {
        body: response,
        options,
        truncated,
        rename_rect: Rect::from_min_max(
            pos2(rect.left() + 48.0 * scale, rect.top() + 4.0 * scale),
            pos2(rect.right() - 52.0 * scale, rect.bottom() - 4.0 * scale),
        ),
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
    // The options button only appears on hover, so only then give up its space.
    let reserved = if response.hovered() { 32.0 } else { 6.0 } * scale;
    let truncated = paint_elided_label(
        painter,
        pos2(text_left, rect.center().y),
        label,
        rect.right() - reserved - text_left,
        FontId::proportional(13.5 * scale),
        if selected {
            Color32::WHITE
        } else {
            Color32::from_rgb(194, 196, 200)
        },
    );
    let options = sidebar_options_button(ui, rect, &response);
    SidebarRowResponse {
        body: response,
        options,
        truncated,
        rename_rect: Rect::from_min_max(
            pos2(text_left, rect.top() + 3.0 * scale),
            pos2(rect.right() - 33.0 * scale, rect.bottom() - 3.0 * scale),
        ),
    }
}

fn sidebar_options_button(ui: &mut egui::Ui, row_rect: Rect, row: &Response) -> Response {
    let scale = sidebar_visual_scale(ui);
    let rect = Rect::from_center_size(
        pos2(row_rect.right() - 16.5 * scale, row_rect.center().y),
        vec2(27.0 * scale, (row_rect.height() - 6.0 * scale).max(1.0)),
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
    context_response: &Response,
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
            action = sidebar_options_contents(
                ui,
                can_reset_name,
                remove_label,
                include_file_actions,
                pinned,
            );
        });
    egui::Popup::context_menu(context_response)
        .width(172.0)
        .frame(
            Frame::new()
                .fill(Color32::from_rgb(29, 31, 37))
                .stroke(Stroke::new(1.0, Color32::from_rgb(61, 64, 75)))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(5)),
        )
        .show(|ui| {
            if action.is_none() {
                action = sidebar_options_contents(
                    ui,
                    can_reset_name,
                    remove_label,
                    include_file_actions,
                    pinned,
                );
            }
        });
    action
}

fn sidebar_options_contents(
    ui: &mut egui::Ui,
    can_reset_name: bool,
    remove_label: Option<&str>,
    include_file_actions: bool,
    pinned: bool,
) -> Option<SidebarMenuAction> {
    let action = if include_file_actions
        && popup_menu_row(ui, "Open with...", Color32::from_rgb(225, 227, 234))
    {
        Some(SidebarMenuAction::OpenWith)
    } else if include_file_actions
        && popup_menu_row(ui, "Open in Files", Color32::from_rgb(225, 227, 234))
    {
        Some(SidebarMenuAction::OpenInFiles)
    } else if popup_menu_row(
        ui,
        if pinned { "Unpin" } else { "Pin to top" },
        Color32::from_rgb(225, 227, 234),
    ) {
        Some(if pinned {
            SidebarMenuAction::Unpin
        } else {
            SidebarMenuAction::Pin
        })
    } else if popup_menu_row(ui, "Rename display name", Color32::from_rgb(225, 227, 234)) {
        Some(SidebarMenuAction::Rename)
    } else if can_reset_name
        && popup_menu_row(ui, "Reset display name", Color32::from_rgb(225, 227, 234))
    {
        Some(SidebarMenuAction::ResetName)
    } else if let Some(remove_label) = remove_label {
        ui.add_space(3.0);
        ui.separator();
        ui.add_space(3.0);
        popup_menu_row(ui, remove_label, Color32::from_rgb(238, 126, 132))
            .then_some(SidebarMenuAction::Remove)
    } else {
        None
    };
    if action.is_some() {
        ui.close();
    }
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
        FontId::proportional(12.0 * scale),
        accent,
    );
    ui.painter().text(
        pos2(rect.right(), rect.center().y),
        Align2::RIGHT_CENTER,
        subtitle,
        FontId::proportional(11.5 * scale),
        SIDEBAR_COUNT,
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

/// Paint a one-line label left-aligned at `left_center`, using the whole
/// `max_width` and eliding with "…" only when the measured text overflows.
/// Returns whether the label was elided.
fn paint_elided_label(
    painter: &egui::Painter,
    left_center: egui::Pos2,
    text: &str,
    max_width: f32,
    font_id: FontId,
    color: Color32,
) -> bool {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat {
            font_id,
            color,
            ..Default::default()
        },
    );
    job.wrap = egui::text::TextWrapping {
        max_width: max_width.max(0.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = painter.layout_job(job);
    let elided = galley.elided;
    painter.galley(
        pos2(left_center.x, left_center.y - galley.size().y / 2.0),
        galley,
        color,
    );
    elided
}

fn elide(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let keep = max_chars.saturating_sub(3);
    format!("{}...", text.chars().take(keep).collect::<String>())
}

fn current_pdf_default_application() -> Option<String> {
    let output = Command::new("xdg-mime")
        .args(["query", "default", "application/pdf"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|desktop_id| !desktop_id.is_empty())
}

fn normalized_chat_title(text: &str) -> Option<String> {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return None;
    }
    Some(compact.chars().take(160).collect())
}

fn restore_legacy_chat_title(session: &mut StudyChatSession) {
    let Some(prefix) = session.title.strip_suffix("...") else {
        return;
    };
    let replacement = session
        .messages
        .values()
        .flat_map(|messages| messages.iter())
        .filter(|message| message.role == ChatRole::User)
        .filter_map(|message| normalized_chat_title(&message.text))
        .find(|title| title.starts_with(prefix));
    if let Some(title) = replacement {
        session.title = title;
    }
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

fn full_document_text_selection(
    text_layers: &HashMap<usize, Vec<TextGlyph>>,
    page_count: usize,
) -> Option<TextSelection> {
    let first = (0..page_count).find_map(|page| {
        (!text_layers.get(&page)?.is_empty()).then_some(TextPosition { page, glyph: 0 })
    })?;
    let last = (0..page_count).rev().find_map(|page| {
        let glyphs = text_layers.get(&page)?;
        if glyphs.is_empty() {
            None
        } else {
            Some(TextPosition {
                page,
                glyph: glyphs.len() - 1,
            })
        }
    })?;
    Some(TextSelection {
        anchor: first,
        focus: last,
    })
}

fn nearest_glyph(
    glyphs: &[TextGlyph],
    page_rect: Rect,
    source_size: [f32; 2],
    pointer: egui::Pos2,
) -> Option<usize> {
    glyphs.iter().enumerate().find_map(|(index, glyph)| {
        glyph_screen_rect(glyph, page_rect, source_size)
            .filter(|rect| rect.contains(pointer))
            .map(|_| index)
    })
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
                    // Search matches are violet so they remain visually distinct from
                    // user-created highlights, whose default ink is warm yellow.
                    Color32::from_rgba_unmultiplied(176, 104, 255, 92),
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

fn selection_action_bar(
    ctx: &egui::Context,
    selection_rect: Rect,
    show_study_chat: bool,
) -> Option<SelectionAction> {
    let mut action = None;
    egui::Area::new(egui::Id::new("text-selection-actions"))
        .order(egui::Order::Tooltip)
        .fixed_pos(pos2(
            selection_rect.center().x - 92.0,
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
                        if show_study_chat && ui.small_button("Ask").clicked() {
                            action = Some(SelectionAction::Chat);
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
    Folder,
    CollapseUp,
    Settings,
    Reader,
    Present,
    Attach,
    Capture,
    Send,
    NewChat,
    DeleteChat,
    Rename,
    Confirm,
    Copy,
    Close,
    Sidebar,
    Search,
    Chat,
    More,
    Plus,
    ZoomIn,
    ZoomOut,
    FitWidth,
    Refresh,
    FolderPlus,
    OpenFile,
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

const SETTINGS_BG: Color32 = Color32::from_rgb(32, 32, 32);
const SETTINGS_NAV_BG: Color32 = Color32::from_rgb(26, 26, 26);
const SETTINGS_CARD: Color32 = Color32::from_rgb(42, 42, 42);
const SETTINGS_OK: Color32 = Color32::from_rgb(120, 200, 140);
const SETTINGS_ACCENT: Color32 = Color32::from_rgb(120, 180, 255);

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsPage {
    General,
    Appearance,
    StudyChat,
}

impl SettingsPage {
    fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Appearance => "Appearance",
            Self::StudyChat => "Study chat",
        }
    }
}

fn settings_nav_item(ui: &mut egui::Ui, label: &str, selected: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
    if selected || response.hovered() {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(7),
            if selected {
                Color32::from_rgb(48, 48, 48)
            } else {
                Color32::from_rgb(38, 38, 38)
            },
        );
    }
    if selected {
        ui.painter().rect_filled(
            Rect::from_min_size(
                rect.left_top() + vec2(0.0, 9.0),
                vec2(3.0, rect.height() - 18.0),
            ),
            CornerRadius::same(2),
            SETTINGS_ACCENT,
        );
    }
    ui.painter().text(
        rect.left_center() + vec2(14.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(14.0),
        if selected {
            Color32::from_rgb(245, 245, 245)
        } else {
            Color32::from_rgb(190, 190, 190)
        },
    );
    response
}

fn settings_card(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    Frame::new()
        .fill(SETTINGS_CARD)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            add_contents(ui);
        });
}

fn settings_card_title(ui: &mut egui::Ui, title: &str, description: &str) {
    ui.label(
        RichText::new(title)
            .size(14.0)
            .strong()
            .color(Color32::from_rgb(236, 236, 236)),
    );
    ui.label(RichText::new(description).size(12.0).color(theme::MUTED));
}

fn settings_divider(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, 0.0, Color32::from_rgb(54, 54, 54));
}

/// A settings row: title and optional description on the left, its control
/// right-aligned on the same line.
fn settings_row(
    ui: &mut egui::Ui,
    title: &str,
    description: &str,
    add_control: impl FnOnce(&mut egui::Ui),
) {
    ui.horizontal(|ui| {
        ui.set_min_height(32.0);
        let control_width = (ui.available_width() * 0.5).min(300.0);
        ui.vertical(|ui| {
            ui.set_width(ui.available_width() - control_width - 12.0);
            ui.label(
                RichText::new(title)
                    .size(13.5)
                    .color(Color32::from_rgb(230, 230, 230)),
            );
            if !description.is_empty() {
                ui.label(RichText::new(description).size(11.5).color(theme::MUTED));
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), add_control);
    });
}

/// A slider with a visible track and its value shown to the right. Call it
/// inside a settings row, whose control area is laid out right-to-left.
fn settings_slider(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    step: f64,
    format: impl Fn(f32) -> String,
) -> Response {
    ui.add_sized(
        [54.0, 20.0],
        egui::Label::new(
            RichText::new(format(*value))
                .size(12.5)
                .color(Color32::from_rgb(200, 200, 200)),
        ),
    );
    ui.scope(|ui| {
        let visuals = ui.visuals_mut();
        visuals.widgets.inactive.bg_fill = Color32::from_rgb(74, 74, 74);
        visuals.widgets.inactive.fg_stroke.color = Color32::from_rgb(235, 235, 235);
        visuals.widgets.hovered.fg_stroke.color = Color32::WHITE;
        visuals.selection.bg_fill = SETTINGS_ACCENT;
        visuals.slider_trailing_fill = true;
        ui.add(
            egui::Slider::new(value, range)
                .step_by(step)
                .show_value(false),
        )
    })
    .inner
}

fn toggle_switch(ui: &mut egui::Ui, on: &mut bool) -> Response {
    let size = vec2(38.0, 20.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(response.id, *on);
    let track = Color32::from_rgb(80, 80, 80).lerp_to_gamma(SETTINGS_ACCENT, t);
    ui.painter()
        .rect_filled(rect, CornerRadius::same(10), track);
    let knob_x = egui::lerp(rect.left() + 10.0..=rect.right() - 10.0, t);
    ui.painter().circle_filled(
        pos2(knob_x, rect.center().y),
        7.0,
        if *on {
            Color32::from_rgb(20, 20, 20)
        } else {
            Color32::from_rgb(220, 220, 220)
        },
    );
    response
}

fn settings_segmented<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    value: &mut T,
    options: &[(T, &str)],
) -> bool {
    let mut changed = false;
    Frame::new()
        .fill(Color32::from_rgb(30, 30, 30))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                // Inside a right-to-left row, add the options in reverse so
                // they still read left to right.
                let reversed = ui.layout().prefer_right_to_left();
                let ordered: Vec<_> = if reversed {
                    options.iter().rev().collect()
                } else {
                    options.iter().collect()
                };
                for (option, label) in ordered {
                    let selected = *value == *option;
                    let response = ui.add(
                        egui::Button::new(RichText::new(*label).size(12.5).color(if selected {
                            Color32::from_rgb(245, 245, 245)
                        } else {
                            Color32::from_rgb(170, 170, 170)
                        }))
                        .fill(if selected {
                            Color32::from_rgb(64, 64, 64)
                        } else {
                            Color32::TRANSPARENT
                        })
                        .stroke(Stroke::NONE)
                        .corner_radius(CornerRadius::same(6)),
                    );
                    if response.clicked() && !selected {
                        *value = *option;
                        changed = true;
                    }
                }
            });
        });
    changed
}

/// Shared surface for every dialog: the Settings panel's colour, corners
/// and shadow.
fn dialog_frame() -> Frame {
    Frame::new()
        .fill(SETTINGS_BG)
        .stroke(Stroke::new(1.0, Color32::from_rgb(58, 58, 58)))
        .corner_radius(CornerRadius::same(12))
        .shadow(egui::Shadow {
            offset: [0, 10],
            blur: 32,
            spread: 0,
            color: Color32::from_black_alpha(140),
        })
        .inner_margin(Margin {
            left: 20,
            right: 14,
            top: 12,
            bottom: 18,
        })
}

/// Title row with a close button. Returns true when × was clicked.
fn dialog_header(ui: &mut egui::Ui, title: &str) -> bool {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(title)
                .size(16.0)
                .strong()
                .color(Color32::from_rgb(240, 240, 240)),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            chrome_button(ui, ToolbarIcon::Close, false, true, "Close (Esc)").clicked()
        })
        .inner
    })
    .inner
}

/// A centred dialog over the dimmed app. Returns true when it should close:
/// ×, Esc, or a click outside when `dismiss_on_backdrop` (left off for
/// dialogs holding typed text, so a stray click never discards it).
fn modal_dialog(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    width: f32,
    dismiss_on_backdrop: bool,
    add_body: impl FnOnce(&mut egui::Ui),
) -> bool {
    let mut close = false;
    let response = egui::Modal::new(egui::Id::new(id))
        .backdrop_color(Color32::from_black_alpha(130))
        .frame(dialog_frame())
        .show(ctx, |ui| {
            ui.set_width(width);
            close |= dialog_header(ui, title);
            ui.add_space(4.0);
            ui.spacing_mut().item_spacing.y = 8.0;
            add_body(ui);
        });
    let escape = response.is_top_modal
        && !response.any_popup_open
        && ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape));
    close || escape || (dismiss_on_backdrop && response.backdrop_response.clicked())
}

/// A dialog that floats beside what it edits, without dimming the app.
/// Returns true on × or Esc.
fn popover_dialog(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    position: Option<egui::Pos2>,
    width: f32,
    add_body: impl FnOnce(&mut egui::Ui),
) -> bool {
    let mut close = false;
    let mut window = egui::Window::new(title)
        .id(egui::Id::new(id))
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .order(egui::Order::Foreground)
        .frame(dialog_frame());
    window = match position {
        Some(position) => window.current_pos(position),
        None => window.anchor(Align2::CENTER_CENTER, vec2(0.0, -40.0)),
    };
    window.show(ctx, |ui| {
        ui.set_width(width);
        close |= dialog_header(ui, title);
        ui.add_space(4.0);
        ui.spacing_mut().item_spacing.y = 8.0;
        add_body(ui);
    });
    close || ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape))
}

fn dialog_text(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.label(
        RichText::new(text.into())
            .size(13.5)
            .color(Color32::from_rgb(214, 214, 214)),
    );
}

fn dialog_hint(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.label(RichText::new(text.into()).size(12.0).color(theme::MUTED));
}

fn dialog_field_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(12.5)
            .color(Color32::from_rgb(200, 200, 200)),
    );
}

/// Right-aligned action row. Add the primary action first: it lands
/// rightmost, with secondary actions to its left.
fn dialog_actions(ui: &mut egui::Ui, add_actions: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(6.0);
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), 34.0),
        Layout::right_to_left(Align::Center),
        add_actions,
    );
}

fn primary_button(ui: &mut egui::Ui, label: &str, enabled: bool) -> Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(label).size(13.5).color(theme::ON_ACCENT))
            .fill(theme::ACCENT_BLUE)
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(8))
            .min_size(vec2(84.0, 32.0)),
    )
}

fn secondary_button(ui: &mut egui::Ui, label: &str) -> Response {
    ui.add(
        egui::Button::new(
            RichText::new(label)
                .size(13.5)
                .color(Color32::from_rgb(225, 225, 225)),
        )
        .fill(Color32::from_rgb(52, 52, 52))
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(8))
        .min_size(vec2(76.0, 32.0)),
    )
}

/// Scale a chrome region by its user-adjustable ratio, independent of the
/// overall interface zoom.
fn apply_region_scale(ui: &mut egui::Ui, factor: f32) {
    ui.style_mut().spacing.item_spacing *= factor;
    ui.style_mut().spacing.button_padding *= factor;
    ui.style_mut().spacing.interact_size *= factor;
    for font in ui.style_mut().text_styles.values_mut() {
        font.size *= factor;
    }
}

/// Square, flat Edge-style toolbar button: no frame at rest, a soft rounded
/// fill on hover and a stronger one when the control is toggled on.
fn chrome_button(
    ui: &mut egui::Ui,
    icon: ToolbarIcon,
    selected: bool,
    enabled: bool,
    help: &str,
) -> Response {
    let scale = toolbar_visual_scale(ui);
    // Shorter rows, such as the document toolbar, get proportionally shorter buttons.
    let height = (30.0 * scale).min(ui.max_rect().height());
    let (rect, response) = ui.allocate_exact_size(vec2(32.0 * scale, height), Sense::click());
    let hovered = enabled && response.hovered();
    if selected || hovered {
        ui.painter().rect_filled(
            rect.shrink(1.0),
            CornerRadius::same(6),
            if selected {
                theme::ACCENT_BLUE.gamma_multiply(0.28)
            } else {
                Color32::from_rgb(54, 54, 54)
            },
        );
    }
    let color = if !enabled {
        Color32::from_rgb(92, 92, 92)
    } else if selected {
        theme::ACCENT_BLUE_TEXT
    } else if hovered {
        Color32::from_rgb(240, 240, 240)
    } else {
        Color32::from_rgb(222, 222, 222)
    };
    paint_toolbar_icon(ui.painter(), rect.center(), icon, color);
    if help.is_empty() {
        response
    } else {
        response.on_hover_text(help)
    }
}

fn toolbar_tool_button(
    ui: &mut egui::Ui,
    mode: &mut ToolMode,
    value: ToolMode,
    icon: ToolbarIcon,
    help: &str,
) -> Response {
    let selected = *mode == value;
    let response = chrome_button(ui, icon, selected, true, help);
    if response.clicked() {
        *mode = if selected { ToolMode::Select } else { value };
    }
    response
}

/// The annotation colour as a round swatch with presets and a full picker.
fn annotation_color_dot(ui: &mut egui::Ui, color: &mut Color32) {
    let scale = toolbar_visual_scale(ui);
    let height = (30.0 * scale).min(ui.max_rect().height());
    let (rect, response) = ui.allocate_exact_size(vec2(28.0 * scale, height), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(
            rect.shrink(1.0),
            CornerRadius::same(6),
            Color32::from_rgb(54, 54, 54),
        );
    }
    ui.painter().circle(
        rect.center(),
        6.5 * scale,
        *color,
        Stroke::new(1.5, Color32::from_rgb(230, 230, 230)),
    );
    let response = response.on_hover_text("Annotation colour");
    egui::Popup::menu(&response).show(|ui| {
        ui.horizontal(|ui| {
            for preset in [
                Color32::from_rgb(255, 211, 74),
                Color32::from_rgb(120, 220, 120),
                Color32::from_rgb(110, 190, 255),
                Color32::from_rgb(255, 140, 190),
                Color32::from_rgb(255, 120, 90),
                Color32::from_rgb(190, 150, 255),
            ] {
                let (dot, clicked) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                ui.painter().circle(
                    dot.center(),
                    8.0,
                    preset,
                    if preset == *color {
                        Stroke::new(2.0, Color32::WHITE)
                    } else {
                        Stroke::NONE
                    },
                );
                if clicked.clicked() {
                    *color = preset;
                }
            }
        });
        ui.add_space(4.0);
        egui::color_picker::color_picker_color32(ui, color, egui::color_picker::Alpha::Opaque);
    });
}

fn toolbar_divider(ui: &mut egui::Ui) {
    let scale = toolbar_visual_scale(ui);
    let height = (30.0 * scale).min(ui.max_rect().height());
    let (rect, _) = ui.allocate_exact_size(vec2(11.0 * scale, height), Sense::hover());
    ui.painter().line_segment(
        [
            pos2(rect.center().x, rect.top() + 7.0 * scale),
            pos2(rect.center().x, rect.bottom() - 7.0 * scale),
        ],
        Stroke::new(1.0, Color32::from_rgb(70, 70, 70)),
    );
}

/// A compact icon-only action for the Study chat composer. Unlike text or emoji
/// buttons, it has no font fallback dependency and therefore cannot turn into a
/// missing-glyph square on a minimal desktop installation.
fn chat_icon_button(ui: &mut egui::Ui, icon: ToolbarIcon, enabled: bool, help: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(30.0, 28.0), Sense::click());
    let hovered = enabled && response.hovered();
    let color = if !enabled {
        Color32::from_rgb(88, 88, 88)
    } else if hovered {
        Color32::from_rgb(245, 245, 245)
    } else {
        Color32::from_rgb(196, 196, 196)
    };
    if hovered {
        ui.painter()
            .rect_filled(rect.shrink(1.0), CornerRadius::same(7), CHAT_HOVER);
    }
    paint_toolbar_icon(ui.painter(), rect.center(), icon, color);
    response.on_hover_text(help)
}

/// Render panel contents inside the exact rectangle granted by a side panel.
/// This prevents a long label, message, or horizontal button row from feeding
/// a larger intrinsic width back into egui's panel state after a resize drag.
fn bounded_panel_content<R>(
    ui: &mut egui::Ui,
    id: impl egui::AsIdSalt,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let rect = ui.available_rect_before_wrap();
    ui.allocate_rect(rect, Sense::hover());
    let mut content = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(id)
            .max_rect(rect)
            .layout(Layout::top_down(Align::Min)),
    );
    content.set_clip_rect(rect);
    add_contents(&mut content)
}

const CHAT_CANVAS: Color32 = theme::SIDEBAR_BG;
const CHAT_HEADER: Color32 = theme::SIDEBAR_BG;
const CHAT_DIVIDER: Color32 = Color32::from_rgb(48, 48, 48);
/// Raised surfaces inside the chat panel: composer, chips, prompt bubbles.
const CHAT_FIELD: Color32 = Color32::from_rgb(52, 52, 52);
/// Hover highlight for buttons on the panel or inside the composer.
const CHAT_HOVER: Color32 = Color32::from_rgb(66, 66, 66);
const CHAT_ACCENT: Color32 = Color32::from_rgb(16, 163, 127);
const CHAT_TEXT: Color32 = Color32::from_rgb(236, 236, 236);
const CHAT_MUTED: Color32 = Color32::from_rgb(166, 166, 166);

fn chat_control_text_size(base: f32) -> f32 {
    (base * 0.78).clamp(10.5, 22.0)
}

fn chat_meta_text_size(base: f32) -> f32 {
    (base * 0.68).clamp(9.5, 19.0)
}

fn apply_chat_typography(ui: &mut egui::Ui, base: f32) {
    let style = ui.style_mut();
    style
        .text_styles
        .insert(egui::TextStyle::Body, FontId::proportional(base));
    style.text_styles.insert(
        egui::TextStyle::Button,
        FontId::proportional(chat_control_text_size(base)),
    );
    style.text_styles.insert(
        egui::TextStyle::Small,
        FontId::proportional(chat_meta_text_size(base)),
    );
    style.text_styles.insert(
        egui::TextStyle::Heading,
        FontId::proportional((base + 1.5).clamp(13.0, 30.0)),
    );
    style
        .text_styles
        .insert(egui::TextStyle::Monospace, FontId::monospace(base));
}

fn chat_empty_state(ui: &mut egui::Ui, text_size: f32) {
    ui.add_space(22.0);
    ui.vertical_centered(|ui| {
        chat_assistant_mark(ui, 42.0);
        ui.add_space(10.0);
        ui.label(
            RichText::new("What are you studying?")
                .size((text_size + 1.5).clamp(13.0, 30.0))
                .strong()
                .color(CHAT_TEXT),
        );
        ui.add_space(4.0);
        ui.add(
            egui::Label::new(
                RichText::new(
                    "Ask a question, attach a PDF, or capture a figure without leaving the page.",
                )
                .size(chat_control_text_size(text_size))
                .color(CHAT_MUTED),
            )
            .wrap()
            .halign(Align::Center),
        );
    });
}

fn chat_status_visible(status: &str, busy: bool) -> bool {
    let normalized = status.trim();
    if normalized.is_empty() {
        return false;
    }
    busy || !(normalized.eq_ignore_ascii_case("ready")
        || normalized.to_ascii_lowercase().ends_with(" is ready."))
}

fn chat_status(ui: &mut egui::Ui, status: &str, busy: bool, text_size: f32) {
    ui.horizontal(|ui| {
        ui.add_space(34.0);
        if busy {
            ui.add(egui::Spinner::new().size(11.0).color(CHAT_ACCENT));
        } else {
            let (rect, _) = ui.allocate_exact_size(vec2(11.0, 11.0), Sense::hover());
            ui.painter().circle_filled(rect.center(), 3.0, CHAT_ACCENT);
        }
        ui.add(
            egui::Label::new(
                RichText::new(status)
                    .size(chat_meta_text_size(text_size))
                    .color(CHAT_MUTED),
            )
            .wrap(),
        );
    });
}

fn chat_message(ui: &mut egui::Ui, message: &ChatMessage, streaming: bool, text_size: f32) -> bool {
    let mut copied = false;
    let available = ui.available_width().max(1.0);
    let edge_margin = 16.0_f32.min(available * 0.06);
    // Keep a comfortable margin on wide panels without leaving most of the
    // panel empty: the column grows with the panel beyond a 760 px minimum.
    let readable_width = (available - edge_margin * 2.0).clamp(1.0, (available * 0.86).max(760.0));
    let leading = ((available - readable_width) * 0.5).max(0.0);
    match message.role {
        ChatRole::User => {
            let bubble_width = chat_user_bubble_width(&message.text, readable_width, text_size);
            ui.horizontal(|ui| {
                ui.add_space((leading + readable_width - bubble_width).max(0.0));
                ui.vertical(|ui| {
                    ui.set_width(bubble_width);
                    Frame::new()
                        .fill(CHAT_FIELD)
                        .corner_radius(CornerRadius::same(18))
                        .inner_margin(Margin::symmetric(13, 10))
                        .show(ui, |ui| {
                            ui.set_width((bubble_width - 26.0).max(1.0));
                            chat_message_attachments(ui, message, text_size);
                            render_chat_markdown(ui, &message.text, text_size);
                        });
                    ui.allocate_ui_with_layout(
                        vec2(bubble_width, 23.0),
                        Layout::right_to_left(Align::Center),
                        |ui| {
                            if chat_copy_button(ui, "Copy prompt", text_size).clicked() {
                                ui.ctx().copy_text(message.text.clone());
                                copied = true;
                            }
                        },
                    );
                });
            });
        }
        ChatRole::Assistant => {
            ui.horizontal(|ui| {
                ui.add_space(leading);
                chat_assistant_mark(ui, 20.0);
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.set_width((readable_width - 28.0).max(1.0));
                    if streaming {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().size(13.0).color(CHAT_ACCENT));
                            ui.label(
                                RichText::new("Generating…")
                                    .size(chat_control_text_size(text_size))
                                    .color(CHAT_MUTED),
                            );
                        });
                    } else {
                        chat_message_attachments(ui, message, text_size);
                        render_chat_markdown(ui, &message.text, text_size);
                        ui.add_space(4.0);
                        if chat_copy_button(ui, "Copy reply", text_size).clicked() {
                            ui.ctx().copy_text(message.text.clone());
                            copied = true;
                        }
                    }
                });
            });
        }
    }
    copied
}

fn chat_assistant_mark(ui: &mut egui::Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let radius = size * 0.48;
    ui.painter()
        .circle_filled(rect.center(), radius, CHAT_ACCENT);
    ui.painter().circle_stroke(
        rect.center(),
        radius,
        Stroke::new(1.0, Color32::from_rgb(55, 207, 167)),
    );
    let center = rect.center();
    let long = size * 0.22;
    let short = size * 0.08;
    let color = Color32::WHITE;
    ui.painter().line_segment(
        [center - vec2(0.0, long), center + vec2(0.0, long)],
        Stroke::new((size * 0.065).max(1.2), color),
    );
    ui.painter().line_segment(
        [center - vec2(long, 0.0), center + vec2(long, 0.0)],
        Stroke::new((size * 0.065).max(1.2), color),
    );
    ui.painter().line_segment(
        [center - vec2(short, short), center + vec2(short, short)],
        Stroke::new((size * 0.045).max(1.0), color.gamma_multiply(0.9)),
    );
    ui.painter().line_segment(
        [center + vec2(-short, short), center + vec2(short, -short)],
        Stroke::new((size * 0.045).max(1.0), color.gamma_multiply(0.9)),
    );
}

fn chat_prompt_rows(prompt: &str, width: f32) -> usize {
    chat_prompt_visual_rows(prompt, width).min(4)
}

fn chat_prompt_visual_rows(prompt: &str, width: f32) -> usize {
    if prompt.is_empty() {
        return 1;
    }
    let characters_per_line = ((width.max(1.0) - 18.0) / 7.2).floor().max(12.0) as usize;
    prompt
        .split('\n')
        .map(|line| line.chars().count().max(1).div_ceil(characters_per_line))
        .sum::<usize>()
        .clamp(1, 512)
}

fn chat_prompt_viewport_height(rows: usize) -> f32 {
    chat_prompt_content_height(rows.clamp(1, 4))
}

fn chat_prompt_content_height(rows: usize) -> f32 {
    22.0 + rows.max(1).saturating_sub(1) as f32 * 18.0
}

fn chat_user_bubble_width(text: &str, available: f32, text_size: f32) -> f32 {
    let available = available.max(1.0);
    let longest_line = text
        .lines()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);
    let estimated = longest_line as f32 * (text_size * 0.53) + 30.0;
    let responsive_max = (available * 0.86).max(1.0).min(available);
    estimated
        .max(74.0_f32.min(available))
        .min(responsive_max)
        .min(680.0)
        .min(available)
}

fn chat_message_attachments(ui: &mut egui::Ui, message: &ChatMessage, text_size: f32) {
    if message.attachments.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for attachment in &message.attachments {
            Frame::new()
                .fill(Color32::from_rgb(57, 57, 57))
                .corner_radius(CornerRadius::same(7))
                .inner_margin(Margin::symmetric(7, 4))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(format!(
                            "{}  {}",
                            attachment.kind_label(),
                            elide(&attachment.name, 26)
                        ))
                        .size(chat_meta_text_size(text_size))
                        .color(Color32::from_rgb(207, 207, 207)),
                    );
                });
        }
    });
    ui.add_space(5.0);
}

fn chat_copy_button(ui: &mut egui::Ui, help: &str, text_size: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(58.0, 24.0), Sense::click());
    let color = if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(7), CHAT_FIELD);
        CHAT_TEXT
    } else {
        CHAT_MUTED
    };
    paint_toolbar_icon(
        ui.painter(),
        pos2(rect.left() + 12.0, rect.center().y),
        ToolbarIcon::Copy,
        color,
    );
    ui.painter().text(
        pos2(rect.left() + 23.0, rect.center().y),
        Align2::LEFT_CENTER,
        "Copy",
        FontId::proportional(chat_meta_text_size(text_size)),
        color,
    );
    response.on_hover_text(help)
}

/// A compact, hover-only copy affordance for structured Markdown output. It
/// is drawn inside the block rectangle so code and tables stay easy to scan
/// while their copy action remains immediately discoverable.
fn chat_block_copy_button(ui: &mut egui::Ui, block_rect: Rect, text: &str) -> bool {
    if !ui.rect_contains_pointer(block_rect) {
        return false;
    }
    let button_rect = Rect::from_min_size(
        pos2(block_rect.right() - 45.0, block_rect.top() + 5.0),
        vec2(39.0, 18.0),
    )
    .intersect(block_rect.shrink(2.0));
    if button_rect.width() < 20.0 || button_rect.height() < 12.0 {
        return false;
    }
    let mut overlay = ui.new_child(
        egui::UiBuilder::new()
            .id_salt((
                "chat-block-copy",
                block_rect.min.x.to_bits(),
                block_rect.min.y.to_bits(),
                text.len(),
            ))
            .max_rect(button_rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    let (rect, response) = overlay.allocate_exact_size(button_rect.size(), Sense::click());
    let fill = if response.hovered() {
        Color32::from_rgb(64, 64, 64)
    } else {
        Color32::from_rgba_unmultiplied(48, 48, 48, 230)
    };
    overlay
        .painter()
        .rect_filled(rect, CornerRadius::same(5), fill);
    paint_toolbar_icon(
        overlay.painter(),
        pos2(rect.left() + 9.0, rect.center().y),
        ToolbarIcon::Copy,
        Color32::from_rgb(205, 205, 205),
    );
    overlay.painter().text(
        pos2(rect.left() + 17.0, rect.center().y),
        Align2::LEFT_CENTER,
        "Copy",
        FontId::proportional(8.5),
        Color32::from_rgb(210, 210, 210),
    );
    if response.clicked() {
        ui.ctx().copy_text(text.to_owned());
        return true;
    }
    false
}

fn chat_send_button(ui: &mut egui::Ui, enabled: bool) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(31.0, 31.0), sense);
    let fill = if enabled {
        if response.hovered() {
            Color32::from_rgb(25, 181, 139)
        } else {
            CHAT_ACCENT
        }
    } else {
        Color32::from_rgb(73, 73, 73)
    };
    ui.painter().circle_filled(rect.center(), 14.0, fill);
    paint_toolbar_icon(
        ui.painter(),
        rect.center(),
        ToolbarIcon::Send,
        if enabled { Color32::WHITE } else { CHAT_MUTED },
    );
    response.on_hover_text("Send message (Enter)")
}

fn chat_action_button(
    ui: &mut egui::Ui,
    label: &str,
    width: f32,
    help: &str,
    text_size: f32,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 28.0), Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(14), CHAT_HOVER);
    }
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(chat_control_text_size(text_size)),
        CHAT_TEXT,
    );
    response.on_hover_text(help)
}

fn chat_history_button(
    ui: &mut egui::Ui,
    label: &str,
    width: f32,
    help: &str,
    text_size: f32,
) -> Response {
    // A visible dropdown so it reads as the conversation switcher, not a caption.
    let (rect, response) = ui.allocate_exact_size(vec2(width, 30.0), Sense::click());
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(8),
        if response.hovered() {
            CHAT_HOVER
        } else {
            CHAT_FIELD
        },
    );
    let font = FontId::proportional(chat_control_text_size(text_size));
    let fitted = fitted_chat_control_text(
        ui.painter(),
        label,
        &font,
        CHAT_TEXT,
        (width - 40.0).max(0.0),
    );
    ui.painter().text(
        pos2(rect.left() + 12.0, rect.center().y),
        Align2::LEFT_CENTER,
        fitted,
        font,
        CHAT_TEXT,
    );
    let center = pos2(rect.right() - 11.0, rect.center().y + 1.0);
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            center + vec2(-3.5, -2.0),
            center + vec2(3.5, -2.0),
            center + vec2(0.0, 2.5),
        ],
        CHAT_MUTED,
        Stroke::NONE,
    ));
    response.on_hover_text(help)
}

fn chat_model_control_widths(available: f32, show_thinking: bool) -> (f32, f32, f32) {
    let available = available.max(1.0);
    let gap = if show_thinking { 6.0 } else { 0.0 };
    let usable = (available - gap).max(1.0);
    let thinking = if show_thinking {
        (available * 0.28).clamp(42.0, 92.0).min(usable * 0.45)
    } else {
        0.0
    };
    let raw_model = (usable - thinking).max(1.0);
    let model = raw_model.min(184.0);
    let spacer = (raw_model - model).max(0.0);
    (model, thinking, spacer)
}

fn fitted_chat_control_text(
    painter: &egui::Painter,
    label: &str,
    font: &FontId,
    color: Color32,
    max_width: f32,
) -> String {
    if painter
        .layout_no_wrap(label.to_owned(), font.clone(), color)
        .size()
        .x
        <= max_width
    {
        return label.to_owned();
    }
    let ellipsis = "…";
    let characters = label.chars().collect::<Vec<_>>();
    for keep in (0..characters.len()).rev() {
        let candidate = format!(
            "{}{}",
            characters[..keep].iter().collect::<String>(),
            ellipsis
        );
        if painter
            .layout_no_wrap(candidate.clone(), font.clone(), color)
            .size()
            .x
            <= max_width
        {
            return candidate;
        }
    }
    String::new()
}

fn chat_dropdown_button(
    ui: &mut egui::Ui,
    label: &str,
    width: f32,
    accent: bool,
    help: &str,
    text_size: f32,
) -> Response {
    // Neutral pills sit flush with the composer and only light up on hover;
    // the accent pill (a call to action such as Connect) stays filled.
    let (rect, response) = ui.allocate_exact_size(vec2(width, 28.0), Sense::click());
    let fill = if accent {
        if response.hovered() {
            Color32::from_rgb(27, 177, 137)
        } else {
            CHAT_ACCENT
        }
    } else if response.hovered() {
        CHAT_HOVER
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, CornerRadius::same(14), fill);
    let text_color = if accent { Color32::WHITE } else { CHAT_TEXT };
    let font = FontId::proportional(chat_control_text_size(text_size));
    let fitted_label = fitted_chat_control_text(
        ui.painter(),
        label,
        &font,
        text_color,
        (width - 32.0).max(0.0),
    );
    ui.painter().text(
        pos2(rect.left() + 12.0, rect.center().y),
        Align2::LEFT_CENTER,
        fitted_label,
        font,
        text_color,
    );
    let center = pos2(rect.right() - 13.0, rect.center().y + 1.0);
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            center + vec2(-3.5, -2.0),
            center + vec2(3.5, -2.0),
            center + vec2(0.0, 2.5),
        ],
        if accent { Color32::WHITE } else { CHAT_MUTED },
        Stroke::NONE,
    ));
    response.on_hover_text(help)
}

fn chat_header_provider_button(
    ui: &mut egui::Ui,
    label: &str,
    text_size: f32,
    help: &str,
) -> Response {
    let font_size = (text_size * 0.96).clamp(12.0, 19.0);
    let font = FontId::proportional(font_size);
    let label_width = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), CHAT_TEXT)
        .size()
        .x;
    let width = (label_width + 34.0).clamp(92.0, 160.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 31.0), Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), CHAT_FIELD);
    }
    ui.painter().text(
        pos2(rect.left() + 13.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font,
        CHAT_TEXT,
    );
    let center = pos2(rect.right() - 13.0, rect.center().y + 1.0);
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            center + vec2(-3.5, -2.0),
            center + vec2(3.5, -2.0),
            center + vec2(0.0, 2.5),
        ],
        CHAT_MUTED,
        Stroke::NONE,
    ));
    response.on_hover_text(help)
}

fn render_chat_markdown(ui: &mut egui::Ui, markdown: &str, base_size: f32) {
    if markdown.is_empty() {
        return;
    }
    let normalized_markdown = normalize_chat_display_text(markdown);
    let markdown = normalized_markdown.as_str();
    let lines = markdown.lines().collect::<Vec<_>>();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim();
        if trimmed.is_empty() {
            ui.add_space(5.0);
            index += 1;
            continue;
        }
        if trimmed.starts_with("```") {
            index += 1;
            let mut code = String::new();
            while index < lines.len() && !lines[index].trim().starts_with("```") {
                if !code.is_empty() {
                    code.push('\n');
                }
                code.push_str(lines[index]);
                index += 1;
            }
            if index < lines.len() {
                index += 1;
            }
            let response = Frame::new()
                .fill(Color32::from_rgb(24, 24, 24))
                .stroke(Stroke::new(1.0, Color32::from_rgb(60, 60, 60)))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin {
                    left: 9,
                    right: 52,
                    top: 9,
                    bottom: 9,
                })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width().max(1.0));
                    ui.add(
                        egui::Label::new(
                            RichText::new(&code)
                                .monospace()
                                .size((base_size - 1.0).max(10.0))
                                .color(Color32::from_rgb(220, 220, 220)),
                        )
                        .wrap()
                        .selectable(true),
                    );
                })
                .response;
            let _ = chat_block_copy_button(ui, response.rect, &code);
            ui.add_space(5.0);
            continue;
        }
        if matches!(trimmed, "---" | "***" | "___") {
            ui.separator();
            index += 1;
            continue;
        }
        if let Some((level, heading)) = markdown_heading(trimmed) {
            render_chat_rich_text(
                ui,
                heading,
                match level {
                    1 => base_size + 6.0,
                    2 => base_size + 4.0,
                    _ => base_size + 2.0,
                },
                Color32::from_rgb(248, 248, 248),
                true,
            );
            ui.add_space(3.0);
            index += 1;
            continue;
        }
        if trimmed.starts_with('>') {
            let mut quote = Vec::new();
            while index < lines.len() && lines[index].trim_start().starts_with('>') {
                quote.push(
                    lines[index]
                        .trim_start()
                        .trim_start_matches('>')
                        .trim_start(),
                );
                index += 1;
            }
            let quote_text = quote.join(" ");
            let quote_response = Frame::new()
                .inner_margin(Margin {
                    left: 10,
                    right: 52,
                    top: 3,
                    bottom: 3,
                })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width().max(1.0));
                    render_chat_rich_text(ui, &quote_text, base_size, CHAT_TEXT, false);
                });
            ui.painter().line_segment(
                [
                    quote_response.response.rect.left_top(),
                    quote_response.response.rect.left_bottom(),
                ],
                Stroke::new(2.0, Color32::from_rgb(104, 104, 104)),
            );
            let _ = chat_block_copy_button(ui, quote_response.response.rect, &quote_text);
            ui.add_space(4.0);
            continue;
        }
        if let Some(table) = markdown_table(&lines, index) {
            let table_response = render_markdown_table(ui, &table, base_size);
            let table_text = markdown_table_text(&table);
            let _ = chat_block_copy_button(ui, table_response.rect, &table_text);
            index += table.consumed;
            ui.add_space(5.0);
            continue;
        }
        if let Some((formula, consumed)) = display_math_block(&lines, index) {
            let rect = render_display_math(ui, &formula, base_size);
            let _ = chat_block_copy_button(ui, rect, &formula);
            index += consumed;
            ui.add_space(3.0);
            continue;
        }
        if let Some(formula) = display_math_line(trimmed) {
            let rect = render_display_math(ui, formula, base_size);
            let _ = chat_block_copy_button(ui, rect, formula);
            index += 1;
            ui.add_space(3.0);
            continue;
        }
        if let Some(item) = markdown_bullet(trimmed) {
            ui.horizontal(|ui| {
                ui.label(RichText::new("•").size(base_size + 2.0).color(CHAT_ACCENT));
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width().max(1.0));
                    render_chat_rich_text(ui, item, base_size, CHAT_TEXT, false);
                });
            });
            index += 1;
            continue;
        }
        if let Some((number, item)) = markdown_ordered_item(trimmed) {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{number}."))
                        .size(base_size)
                        .color(CHAT_ACCENT),
                );
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width().max(1.0));
                    render_chat_rich_text(ui, item, base_size, CHAT_TEXT, false);
                });
            });
            index += 1;
            continue;
        }

        let mut paragraph = vec![trimmed];
        index += 1;
        while index < lines.len()
            && !lines[index].trim().is_empty()
            && !markdown_block_start(lines[index].trim())
        {
            paragraph.push(lines[index].trim());
            index += 1;
        }
        render_chat_rich_text(ui, &paragraph.join(" "), base_size, CHAT_TEXT, false);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MarkdownTable {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    consumed: usize,
}

fn markdown_table(lines: &[&str], start: usize) -> Option<MarkdownTable> {
    let headers = markdown_table_cells(lines.get(start)?)?;
    let separators = markdown_table_cells(lines.get(start + 1)?)?;
    if headers.is_empty()
        || headers.len() != separators.len()
        || !separators.iter().all(|cell| {
            let core = cell.trim().trim_start_matches(':').trim_end_matches(':');
            core.len() >= 3 && core.chars().all(|character| character == '-')
        })
    {
        return None;
    }
    let mut rows = Vec::new();
    let mut consumed = 2;
    while let Some(line) = lines.get(start + consumed) {
        let Some(cells) = markdown_table_cells(line) else {
            break;
        };
        if cells.len() != headers.len() {
            break;
        }
        rows.push(cells);
        consumed += 1;
    }
    Some(MarkdownTable {
        headers,
        rows,
        consumed,
    })
}

fn markdown_table_cells(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return None;
    }
    let body = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let body = body.strip_suffix('|').unwrap_or(body);
    let cells = body
        .split('|')
        .map(|cell| cell.trim().to_owned())
        .collect::<Vec<_>>();
    (cells.len() >= 2).then_some(cells)
}

fn markdown_table_text(table: &MarkdownTable) -> String {
    std::iter::once(&table.headers)
        .chain(table.rows.iter())
        .map(|row| format!("| {} |", row.join(" | ")))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Removes only invalid, invisible control characters. Ordinary Unicode is
/// retained and rendered through the bundled Noto fallbacks above.
fn normalize_chat_display_text(text: &str) -> String {
    text.chars().fold(
        String::with_capacity(text.len()),
        |mut normalized, character| {
            if character.is_control() && !matches!(character, '\n' | '\r' | '\t') {
                normalized.push(' ');
            } else {
                normalized.push(character);
            }
            normalized
        },
    )
}

fn render_markdown_table(ui: &mut egui::Ui, table: &MarkdownTable, base_size: f32) -> Response {
    let columns = table.headers.len().max(1);
    // Reserve the outer one-point border on both sides so the table never widens its chat panel.
    let width = (ui.available_width() - 2.0).max(1.0);
    let cell_width = (width / columns as f32).max(42.0);
    Frame::new()
        .stroke(Stroke::new(1.0, Color32::from_rgb(61, 61, 61)))
        .corner_radius(CornerRadius::same(6))
        .fill(Color32::from_rgb(31, 31, 31))
        .inner_margin(Margin::ZERO)
        .show(ui, |ui| {
            let mut cells = Vec::new();
            let grid = egui::Grid::new(ui.next_auto_id())
                .num_columns(columns)
                .striped(false)
                .spacing(egui::Vec2::ZERO)
                .show(ui, |ui| {
                    for (row_index, row) in std::iter::once(&table.headers)
                        .chain(table.rows.iter())
                        .enumerate()
                    {
                        for cell in row {
                            let response = Frame::new()
                                .fill(if row_index == 0 {
                                    Color32::from_rgb(39, 39, 39)
                                } else if row_index % 2 == 0 {
                                    Color32::from_rgb(34, 34, 34)
                                } else {
                                    Color32::from_rgb(30, 30, 30)
                                })
                                .inner_margin(Margin::symmetric(8, 6))
                                .show(ui, |ui| {
                                    ui.set_min_width((cell_width - 16.0).max(1.0));
                                    ui.set_max_width((cell_width - 16.0).max(1.0));
                                    render_chat_rich_text(
                                        ui,
                                        cell,
                                        base_size,
                                        CHAT_TEXT,
                                        row_index == 0,
                                    );
                                })
                                .response;
                            cells.push((row_index, response.rect));
                        }
                        ui.end_row();
                    }
                });

            let border = Stroke::new(1.0, Color32::from_rgb(61, 61, 61));
            for (row_index, rect) in cells {
                if rect.right() < grid.response.rect.right() - 0.5 {
                    ui.painter()
                        .line_segment([rect.right_top(), rect.right_bottom()], border);
                }
                if row_index < table.rows.len() {
                    ui.painter()
                        .line_segment([rect.left_bottom(), rect.right_bottom()], border);
                }
            }
        })
        .response
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChatTextSegment<'a> {
    Text(&'a str),
    Math(&'a str, bool),
}

fn chat_text_segments(text: &str) -> Vec<ChatTextSegment<'_>> {
    let delimiters = [
        (r"\[", r"\]", true),
        ("$$", "$$", true),
        (r"\(", r"\)", false),
        ("$", "$", false),
    ];
    let mut segments = Vec::new();
    let mut cursor = 0;
    while cursor < text.len() {
        let next = delimiters
            .iter()
            .filter_map(|(open, close, display)| {
                text[cursor..]
                    .find(open)
                    .map(|offset| (cursor + offset, *open, *close, *display))
            })
            .filter(|(position, open, _, _)| {
                *open != "$" || *position == 0 || text.as_bytes()[position - 1] != b'\\'
            })
            .min_by_key(|(position, open, _, _)| (*position, usize::MAX - open.len()));
        let Some((start, open, close, display)) = next else {
            segments.push(ChatTextSegment::Text(&text[cursor..]));
            break;
        };
        let body_start = start + open.len();
        let Some(relative_end) = text[body_start..].find(close) else {
            segments.push(ChatTextSegment::Text(&text[cursor..]));
            break;
        };
        let end = body_start + relative_end;
        if start > cursor {
            segments.push(ChatTextSegment::Text(&text[cursor..start]));
        }
        let formula = text[body_start..end].trim();
        if formula.is_empty() {
            segments.push(ChatTextSegment::Text(&text[start..end + close.len()]));
        } else {
            segments.push(ChatTextSegment::Math(formula, display));
        }
        cursor = end + close.len();
    }
    if segments.is_empty() {
        segments.push(ChatTextSegment::Text(text));
    }
    segments
}

fn display_math_line(line: &str) -> Option<&str> {
    if let Some(body) = line
        .strip_prefix(r"\[")
        .and_then(|body| body.strip_suffix(r"\]"))
    {
        return (!body.trim().is_empty()).then_some(body.trim());
    }
    line.strip_prefix("$$")
        .and_then(|body| body.strip_suffix("$$"))
        .map(str::trim)
        .filter(|body| !body.is_empty())
}

fn display_math_block(lines: &[&str], start: usize) -> Option<(String, usize)> {
    let opening = lines.get(start)?.trim();
    let closing = match opening {
        r"\[" => r"\]",
        "$$" => "$$",
        _ => return None,
    };
    let mut formula = Vec::new();
    let mut index = start + 1;
    while let Some(line) = lines.get(index) {
        if line.trim() == closing {
            return (!formula.is_empty()).then(|| (formula.join(" "), index - start + 1));
        }
        formula.push(line.trim());
        index += 1;
    }
    None
}

fn render_chat_rich_text(ui: &mut egui::Ui, text: &str, size: f32, color: Color32, strong: bool) {
    let segments = chat_text_segments(text);
    if !segments
        .iter()
        .any(|segment| matches!(segment, ChatTextSegment::Math(_, _)))
    {
        markdown_label(ui, text, size, color, strong);
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for segment in segments {
            match segment {
                ChatTextSegment::Text(text) if !text.is_empty() => {
                    let mut job = markdown_inline_job(ui, text, size, color, strong);
                    job.wrap.max_width = ui.available_width().max(1.0);
                    ui.add(egui::Label::new(job).wrap().selectable(true));
                }
                ChatTextSegment::Math(formula, display) => {
                    render_chat_math(ui, formula, size, display);
                }
                ChatTextSegment::Text(_) => {}
            }
        }
    });
}

#[derive(Clone)]
struct CachedChatMath {
    texture: TextureHandle,
    size: egui::Vec2,
}

/// Remove every `\boxed{…}` wrapper, keeping its contents. Returns `None`
/// when the formula has no box.
fn strip_boxed(formula: &str) -> Option<String> {
    const BOXED: &str = "\\boxed{";
    if !formula.contains(BOXED) {
        return None;
    }
    let mut output = String::with_capacity(formula.len());
    let mut rest = formula;
    while let Some(start) = rest.find(BOXED) {
        output.push_str(&rest[..start]);
        let inner = &rest[start + BOXED.len()..];
        let mut depth = 1usize;
        let mut end = None;
        for (offset, character) in inner.char_indices() {
            match character {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(offset);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else {
            // Unbalanced braces: leave the remainder untouched.
            output.push_str(&rest[start..]);
            return Some(output);
        };
        output.push_str(&inner[..end]);
        rest = &inner[end + 1..];
    }
    output.push_str(rest);
    Some(output)
}

/// Display math, centered. A model's `\boxed{}` final answer is shown as a
/// softly tinted card instead of LaTeX's hard white frame.
fn render_display_math(ui: &mut egui::Ui, formula: &str, base_size: f32) -> Rect {
    let margin = Margin {
        left: 10,
        right: 52,
        top: 6,
        bottom: 6,
    };
    match strip_boxed(formula) {
        Some(unboxed) => {
            Frame::new()
                .fill(Color32::from_rgb(38, 52, 47))
                .stroke(Stroke::new(1.0, CHAT_ACCENT.gamma_multiply(0.45)))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(margin)
                .show(ui, |ui| render_chat_math(ui, &unboxed, base_size, true))
                .response
                .rect
        }
        None => {
            Frame::new()
                .inner_margin(margin)
                .show(ui, |ui| render_chat_math(ui, formula, base_size, true))
                .response
                .rect
        }
    }
}

fn render_chat_math(ui: &mut egui::Ui, formula: &str, size: f32, display: bool) {
    let cache_id = egui::Id::new(("chat-math", formula, (size * 2.0).round() as i32, display));
    let cached = ui
        .ctx()
        .data_mut(|data| data.get_temp::<CachedChatMath>(cache_id));
    let cached = cached.or_else(|| {
        let font = chat_math_font()?;
        let mut options = PngOptions::new();
        options.font_size_pt = Dim::from_i64(size.round().max(10.0) as i64);
        options.dpi = Dim::from_i64(144);
        options.color = LatexColor::rgb(CHAT_TEXT.r(), CHAT_TEXT.g(), CHAT_TEXT.b());
        options.display = display;
        let png = latex_to_png(formula, font, &options).ok()?;
        let image = image::load_from_memory(&png).ok()?.into_rgba8();
        let dimensions = [image.width() as usize, image.height() as usize];
        let pixels = image.into_raw();
        let texture = ui.ctx().load_texture(
            format!("chat-math-{cache_id:?}"),
            ColorImage::from_rgba_unmultiplied(dimensions, &pixels),
            TextureOptions::LINEAR,
        );
        let rendered = CachedChatMath {
            texture,
            size: vec2(dimensions[0] as f32 / 2.0, dimensions[1] as f32 / 2.0),
        };
        ui.ctx()
            .data_mut(|data| data.insert_temp(cache_id, rendered.clone()));
        Some(rendered)
    });
    if let Some(cached) = cached {
        let max_width = ui.available_width().max(1.0);
        let scale = (max_width / cached.size.x.max(1.0)).min(1.0);
        let image = egui::Image::new((cached.texture.id(), cached.size * scale));
        if display {
            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                ui.add(image);
            });
        } else {
            ui.add(image);
        }
    } else {
        ui.label(
            RichText::new(formula)
                .monospace()
                .size(size)
                .color(CHAT_TEXT),
        );
    }
}

fn chat_math_font() -> Option<&'static MathFont> {
    static FONT: OnceLock<Option<MathFont>> = OnceLock::new();
    FONT.get_or_init(|| MathFont::stix_two_math().ok()).as_ref()
}

fn markdown_heading(line: &str) -> Option<(usize, &str)> {
    let hashes = line
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if (1..=6).contains(&hashes) && line.as_bytes().get(hashes) == Some(&b' ') {
        Some((hashes, line[hashes + 1..].trim()))
    } else {
        None
    }
}

fn markdown_bullet(line: &str) -> Option<&str> {
    ["- ", "* ", "+ "]
        .into_iter()
        .find_map(|prefix| line.strip_prefix(prefix))
}

fn markdown_ordered_item(line: &str) -> Option<(&str, &str)> {
    let (number, body) = line.split_once(". ")?;
    (!number.is_empty() && number.chars().all(|character| character.is_ascii_digit()))
        .then_some((number, body))
}

fn markdown_block_start(line: &str) -> bool {
    line.starts_with("```")
        || matches!(line, "---" | "***" | "___")
        || markdown_heading(line).is_some()
        || line.starts_with('>')
        || markdown_bullet(line).is_some()
        || markdown_ordered_item(line).is_some()
        || display_math_line(line).is_some()
        || matches!(line, r"\[" | "$$")
        || line.contains('|')
}

fn markdown_label(ui: &mut egui::Ui, text: &str, size: f32, color: Color32, strong: bool) {
    let mut job = markdown_inline_job(ui, text, size, color, strong);
    job.wrap.max_width = ui.available_width().max(1.0);
    ui.add(egui::Label::new(job).wrap().selectable(true));
}

fn markdown_inline_job(
    ui: &egui::Ui,
    text: &str,
    size: f32,
    color: Color32,
    base_strong: bool,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(body) = rest.strip_prefix("**")
            && let Some(end) = body.find("**")
        {
            append_markdown_span(
                ui,
                &mut job,
                &body[..end],
                size,
                color,
                true,
                false,
                false,
                false,
            );
            rest = &body[end + 2..];
            continue;
        }
        if let Some(body) = rest.strip_prefix("__")
            && let Some(end) = body.find("__")
        {
            append_markdown_span(
                ui,
                &mut job,
                &body[..end],
                size,
                color,
                true,
                false,
                false,
                false,
            );
            rest = &body[end + 2..];
            continue;
        }
        if let Some(body) = rest.strip_prefix('`')
            && let Some(end) = body.find('`')
        {
            append_markdown_span(
                ui,
                &mut job,
                &body[..end],
                size - 0.5,
                color,
                false,
                false,
                true,
                false,
            );
            rest = &body[end + 1..];
            continue;
        }
        if let Some(body) = rest.strip_prefix("~~")
            && let Some(end) = body.find("~~")
        {
            append_markdown_span(
                ui,
                &mut job,
                &body[..end],
                size,
                color,
                false,
                false,
                false,
                true,
            );
            rest = &body[end + 2..];
            continue;
        }
        if let Some(body) = rest.strip_prefix('*')
            && let Some(end) = body.find('*')
        {
            append_markdown_span(
                ui,
                &mut job,
                &body[..end],
                size,
                color,
                false,
                true,
                false,
                false,
            );
            rest = &body[end + 1..];
            continue;
        }
        if let Some(body) = rest.strip_prefix('_')
            && let Some(end) = body.find('_')
        {
            append_markdown_span(
                ui,
                &mut job,
                &body[..end],
                size,
                color,
                false,
                true,
                false,
                false,
            );
            rest = &body[end + 1..];
            continue;
        }
        if let Some(body) = rest.strip_prefix('[')
            && let Some(label_end) = body.find("](")
            && let Some(url_end) = body[label_end + 2..].find(')')
        {
            let label = &body[..label_end];
            append_markdown_span(
                ui,
                &mut job,
                label,
                size,
                CHAT_ACCENT,
                true,
                false,
                false,
                false,
            );
            rest = &body[label_end + 2 + url_end + 1..];
            continue;
        }

        let marker = rest
            .char_indices()
            .find(|(_, character)| matches!(character, '*' | '_' | '`' | '~' | '['))
            .map(|(index, _)| index)
            .unwrap_or(rest.len());
        let end = if marker == 0 {
            rest.chars()
                .next()
                .map(char::len_utf8)
                .unwrap_or(rest.len())
        } else {
            marker
        };
        append_markdown_span(
            ui,
            &mut job,
            &rest[..end],
            size,
            color,
            base_strong,
            false,
            false,
            false,
        );
        rest = &rest[end..];
    }
    job
}

#[allow(clippy::too_many_arguments)]
fn append_markdown_span(
    ui: &egui::Ui,
    job: &mut egui::text::LayoutJob,
    text: &str,
    size: f32,
    color: Color32,
    strong: bool,
    italics: bool,
    code: bool,
    strikethrough: bool,
) {
    let mut rich = RichText::new(text).size(size).color(color);
    if strong {
        rich = rich.strong();
    }
    if italics {
        rich = rich.italics();
    }
    if code {
        rich = rich
            .monospace()
            .background_color(Color32::from_rgb(55, 55, 55));
    }
    if strikethrough {
        rich = rich.strikethrough();
    }
    rich.append_to(job, ui.style(), egui::FontSelection::Default, Align::Center);
}

fn paint_toolbar_icon(
    painter: &egui::Painter,
    center: egui::Pos2,
    icon: ToolbarIcon,
    color: Color32,
) {
    let stroke = Stroke::new(1.6, color);
    match icon {
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
            // Snip frame: four corner brackets around a scanned text line.
            for (x, y) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let corner = center + vec2(7.0 * x, 6.0 * y);
                painter.line_segment([corner, corner - vec2(4.0 * x, 0.0)], stroke);
                painter.line_segment([corner, corner - vec2(0.0, 4.0 * y)], stroke);
            }
            painter.line_segment([center + vec2(-3.5, 0.0), center + vec2(3.5, 0.0)], stroke);
        }
        ToolbarIcon::Math => {
            // Square-root sign over an x.
            painter.add(egui::Shape::line(
                vec![
                    center + vec2(-8.0, 1.0),
                    center + vec2(-5.5, 0.0),
                    center + vec2(-3.0, 6.0),
                    center + vec2(0.5, -7.0),
                    center + vec2(8.0, -7.0),
                ],
                stroke,
            ));
            painter.line_segment([center + vec2(2.0, -2.5), center + vec2(7.0, 3.5)], stroke);
            painter.line_segment([center + vec2(7.0, -2.5), center + vec2(2.0, 3.5)], stroke);
        }
        ToolbarIcon::Refresh => {
            let radius = 6.0;
            let points: Vec<_> = (0..=20)
                .map(|step| {
                    let angle = -0.6 + step as f32 / 20.0 * 5.0;
                    center + vec2(angle.cos(), angle.sin()) * radius
                })
                .collect();
            let tip = *points.last().expect("arc has points");
            painter.add(egui::Shape::line(points, stroke));
            painter.line_segment([tip, tip + vec2(0.5, -4.0)], stroke);
            painter.line_segment([tip, tip + vec2(4.0, 0.5)], stroke);
        }
        ToolbarIcon::FolderPlus => {
            paint_folder_icon(painter, center + vec2(-1.5, 0.5), color, 0.95);
            let plus = center + vec2(6.5, 5.0);
            painter.circle_filled(plus, 4.6, theme::SIDEBAR_BG);
            painter.line_segment([plus - vec2(3.0, 0.0), plus + vec2(3.0, 0.0)], stroke);
            painter.line_segment([plus - vec2(0.0, 3.0), plus + vec2(0.0, 3.0)], stroke);
        }
        ToolbarIcon::OpenFile => {
            let page = Rect::from_center_size(center + vec2(-1.5, 0.0), vec2(11.0, 14.0));
            painter.add(egui::Shape::closed_line(
                vec![
                    page.left_top(),
                    page.right_top() - vec2(3.5, 0.0),
                    page.right_top() + vec2(0.0, 3.5),
                    page.right_bottom(),
                    page.left_bottom(),
                ],
                stroke,
            ));
            let arrow = center + vec2(6.0, 2.0);
            painter.circle_filled(arrow, 4.8, theme::SIDEBAR_BG);
            painter.line_segment([arrow + vec2(0.0, 3.5), arrow - vec2(0.0, 3.5)], stroke);
            painter.line_segment([arrow - vec2(0.0, 3.5), arrow + vec2(-2.8, -0.8)], stroke);
            painter.line_segment([arrow - vec2(0.0, 3.5), arrow + vec2(2.8, -0.8)], stroke);
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
        ToolbarIcon::Attach => {
            // Paperclip: two nested open curves, drawn as lines so it remains
            // clear at the small composer-control size.
            painter.line_segment(
                [center + vec2(2.0, -7.0), center + vec2(-4.0, -1.0)],
                stroke,
            );
            painter.line_segment(
                [center + vec2(-4.0, -1.0), center + vec2(-4.0, 4.0)],
                stroke,
            );
            painter.line_segment([center + vec2(-4.0, 4.0), center + vec2(-1.3, 6.7)], stroke);
            painter.line_segment([center + vec2(-1.3, 6.7), center + vec2(2.0, 6.7)], stroke);
            painter.line_segment([center + vec2(2.0, 6.7), center + vec2(6.0, 2.7)], stroke);
            painter.line_segment([center + vec2(6.0, 2.7), center + vec2(6.0, -4.2)], stroke);
            painter.line_segment([center + vec2(6.0, -4.2), center + vec2(3.3, -6.8)], stroke);
            painter.line_segment([center + vec2(3.3, -6.8), center + vec2(0.0, -6.8)], stroke);
            painter.line_segment(
                [center + vec2(0.0, -6.8), center + vec2(-2.0, -4.8)],
                stroke,
            );
            painter.line_segment(
                [center + vec2(-2.0, -4.8), center + vec2(-2.0, 2.8)],
                stroke,
            );
        }
        ToolbarIcon::Capture => {
            let corner = 5.8;
            for (x, y, sx, sy) in [
                (-corner, -corner, 1.0, 1.0),
                (corner, -corner, -1.0, 1.0),
                (-corner, corner, 1.0, -1.0),
                (corner, corner, -1.0, -1.0),
            ] {
                let start = center + vec2(x, y + 4.0 * sy);
                let elbow = center + vec2(x, y);
                let end = center + vec2(x + 4.0 * sx, y);
                painter.line_segment([start, elbow], stroke);
                painter.line_segment([elbow, end], stroke);
            }
        }
        ToolbarIcon::Send => {
            let plane = vec![
                center + vec2(-7.0, -5.5),
                center + vec2(7.5, 0.0),
                center + vec2(-7.0, 5.5),
                center + vec2(-3.8, 0.0),
            ];
            painter.add(egui::Shape::convex_polygon(plane, color, Stroke::NONE));
            painter.line_segment(
                [center + vec2(-3.8, 0.0), center + vec2(1.0, 0.0)],
                Stroke::new(1.0, theme::PANEL_BG),
            );
        }
        ToolbarIcon::NewChat => {
            let bubble = Rect::from_center_size(center + vec2(-1.5, -1.0), vec2(13.0, 10.0));
            painter.rect_stroke(bubble, 3.0, stroke, StrokeKind::Inside);
            painter.line_segment(
                [
                    bubble.left_bottom() + vec2(3.5, 0.0),
                    bubble.left_bottom() + vec2(1.5, 3.0),
                ],
                stroke,
            );
            painter.line_segment([center + vec2(4.0, 3.5), center + vec2(8.0, 3.5)], stroke);
            painter.line_segment([center + vec2(6.0, 1.5), center + vec2(6.0, 5.5)], stroke);
        }
        ToolbarIcon::Confirm => {
            painter.line_segment(
                [center + vec2(-6.0, 0.0), center + vec2(-1.8, 4.3)],
                Stroke::new(1.8, color),
            );
            painter.line_segment(
                [center + vec2(-1.8, 4.3), center + vec2(6.2, -5.2)],
                Stroke::new(1.8, color),
            );
        }
        ToolbarIcon::Copy => {
            painter.rect_stroke(
                Rect::from_center_size(center + vec2(2.0, 1.5), vec2(9.0, 10.0)),
                1.5,
                Stroke::new(1.2, color),
                StrokeKind::Inside,
            );
            painter.line_segment(
                [center + vec2(-4.5, -5.5), center + vec2(2.5, -5.5)],
                Stroke::new(1.2, color),
            );
            painter.line_segment(
                [center + vec2(-4.5, -5.5), center + vec2(-4.5, 3.5)],
                Stroke::new(1.2, color),
            );
        }
        ToolbarIcon::Sidebar => {
            let frame = Rect::from_center_size(center, vec2(16.0, 13.0));
            painter.rect_stroke(frame, 2.0, stroke, StrokeKind::Inside);
            painter.line_segment(
                [
                    pos2(frame.left() + 5.5, frame.top()),
                    pos2(frame.left() + 5.5, frame.bottom()),
                ],
                stroke,
            );
        }
        ToolbarIcon::Search => {
            painter.circle_stroke(center + vec2(-1.5, -1.5), 5.0, stroke);
            painter.line_segment([center + vec2(2.2, 2.2), center + vec2(6.5, 6.5)], stroke);
        }
        ToolbarIcon::Chat => {
            let bubble = Rect::from_center_size(center - vec2(0.0, 1.0), vec2(15.0, 11.0));
            painter.rect_stroke(bubble, 3.5, stroke, StrokeKind::Inside);
            painter.line_segment(
                [
                    bubble.left_bottom() + vec2(4.0, 0.0),
                    bubble.left_bottom() + vec2(2.0, 3.5),
                ],
                stroke,
            );
            for x in [-3.5, 0.0, 3.5] {
                painter.circle_filled(center + vec2(x, -1.0), 0.9, color);
            }
        }
        ToolbarIcon::More => {
            for x in [-5.0, 0.0, 5.0] {
                painter.circle_filled(center + vec2(x, 0.0), 1.3, color);
            }
        }
        ToolbarIcon::Plus | ToolbarIcon::ZoomIn => {
            painter.line_segment([center + vec2(-5.5, 0.0), center + vec2(5.5, 0.0)], stroke);
            painter.line_segment([center + vec2(0.0, -5.5), center + vec2(0.0, 5.5)], stroke);
        }
        ToolbarIcon::ZoomOut => {
            painter.line_segment([center + vec2(-5.5, 0.0), center + vec2(5.5, 0.0)], stroke);
        }
        ToolbarIcon::FitWidth => {
            let page = Rect::from_center_size(center, vec2(16.0, 12.0));
            painter.rect_stroke(page, 1.5, stroke, StrokeKind::Inside);
            for (tip, dir) in [(page.left() + 3.0, 1.0), (page.right() - 3.0, -1.0)] {
                painter.line_segment([pos2(tip, center.y), pos2(center.x, center.y)], stroke);
                painter.line_segment(
                    [pos2(tip, center.y), pos2(tip + 2.5 * dir, center.y - 2.5)],
                    stroke,
                );
                painter.line_segment(
                    [pos2(tip, center.y), pos2(tip + 2.5 * dir, center.y + 2.5)],
                    stroke,
                );
            }
        }
        ToolbarIcon::DeleteChat => {
            let body = Rect::from_center_size(center + vec2(0.0, 2.0), vec2(10.0, 11.0));
            painter.rect_stroke(body, 1.5, stroke, StrokeKind::Inside);
            painter.line_segment(
                [center + vec2(-6.0, -5.0), center + vec2(6.0, -5.0)],
                stroke,
            );
            painter.line_segment(
                [center + vec2(-2.5, -7.0), center + vec2(2.5, -7.0)],
                stroke,
            );
            painter.line_segment(
                [center + vec2(-2.0, -1.0), center + vec2(-2.0, 5.0)],
                Stroke::new(1.0, color),
            );
            painter.line_segment(
                [center + vec2(2.0, -1.0), center + vec2(2.0, 5.0)],
                Stroke::new(1.0, color),
            );
        }
        ToolbarIcon::Rename => {
            let tip = center + vec2(-5.7, 5.7);
            let end = center + vec2(5.0, -5.0);
            painter.line_segment([tip, end], Stroke::new(2.4, color));
            painter.line_segment(
                [center + vec2(-7.0, 7.0), center + vec2(-3.2, 6.2)],
                Stroke::new(1.3, color),
            );
            painter.add(egui::Shape::convex_polygon(
                vec![
                    center + vec2(4.2, -6.0),
                    center + vec2(6.5, -3.7),
                    center + vec2(6.8, -6.8),
                ],
                color,
                Stroke::NONE,
            ));
        }
        ToolbarIcon::Highlight => {
            // Outline marker over a yellow stroke; the colour is the cue.
            painter.add(egui::Shape::closed_line(
                vec![
                    center + vec2(-6.0, 3.0),
                    center + vec2(1.0, -4.0),
                    center + vec2(5.0, 0.0),
                    center + vec2(-2.0, 7.0),
                ],
                stroke,
            ));
            painter.add(egui::Shape::closed_line(
                vec![
                    center + vec2(1.0, -4.0),
                    center + vec2(3.5, -6.5),
                    center + vec2(7.5, -2.5),
                    center + vec2(5.0, 0.0),
                ],
                stroke,
            ));
            painter.line_segment(
                [center + vec2(-7.5, 8.5), center + vec2(1.5, 8.5)],
                Stroke::new(2.4, Color32::from_rgb(255, 211, 59)),
            );
        }
        ToolbarIcon::Pen => {
            painter.add(egui::Shape::closed_line(
                vec![
                    center + vec2(-7.0, 7.0),
                    center + vec2(-6.3, 2.1),
                    center + vec2(2.9, -7.1),
                    center + vec2(7.1, -2.9),
                    center + vec2(-2.1, 6.3),
                ],
                stroke,
            ));
            painter.line_segment([center + vec2(-6.3, 2.1), center + vec2(-2.1, 6.3)], stroke);
            painter.line_segment([center + vec2(1.5, -5.7), center + vec2(5.7, -1.5)], stroke);
        }
        ToolbarIcon::Eraser => {
            painter.add(egui::Shape::closed_line(
                vec![
                    center + vec2(-7.8, 3.1),
                    center + vec2(2.1, -6.8),
                    center + vec2(7.8, -1.1),
                    center + vec2(-2.1, 8.8),
                ],
                stroke,
            ));
            painter.line_segment([center + vec2(-3.8, -0.8), center + vec2(1.8, 4.8)], stroke);
        }
        ToolbarIcon::Print => {
            // Printer: paper feeding in on top, body, printed sheet below.
            painter.add(egui::Shape::line(
                vec![
                    center + vec2(-4.5, -3.0),
                    center + vec2(-4.5, -8.0),
                    center + vec2(4.5, -8.0),
                    center + vec2(4.5, -3.0),
                ],
                stroke,
            ));
            painter.rect_stroke(
                Rect::from_min_max(center + vec2(-8.0, -3.0), center + vec2(8.0, 4.0)),
                2.0,
                stroke,
                StrokeKind::Middle,
            );
            painter.rect_stroke(
                Rect::from_min_max(center + vec2(-4.5, 1.0), center + vec2(4.5, 8.5)),
                0.5,
                stroke,
                StrokeKind::Middle,
            );
            painter.circle_filled(center + vec2(5.5, -0.5), 1.0, color);
        }
        ToolbarIcon::Export => {
            // Save a copy: arrow down into a tray.
            painter.add(egui::Shape::line(
                vec![
                    center + vec2(-7.5, 2.0),
                    center + vec2(-7.5, 7.5),
                    center + vec2(7.5, 7.5),
                    center + vec2(7.5, 2.0),
                ],
                stroke,
            ));
            painter.line_segment([center + vec2(0.0, -8.0), center + vec2(0.0, 3.0)], stroke);
            painter.line_segment([center + vec2(0.0, 3.0), center + vec2(-3.8, -0.8)], stroke);
            painter.line_segment([center + vec2(0.0, 3.0), center + vec2(3.8, -0.8)], stroke);
        }
        ToolbarIcon::Reader => {
            // Open book.
            for side in [-1.0_f32, 1.0] {
                painter.add(egui::Shape::line(
                    vec![
                        center + vec2(0.0, -5.0),
                        center + vec2(7.5 * side, -7.0),
                        center + vec2(7.5 * side, 6.0),
                        center + vec2(0.0, 8.0),
                    ],
                    stroke,
                ));
            }
            painter.line_segment([center + vec2(0.0, -5.0), center + vec2(0.0, 8.0)], stroke);
        }
        ToolbarIcon::Present => {
            // Screen with a play mark.
            let screen = Rect::from_center_size(center + vec2(0.0, -1.5), vec2(17.0, 12.0));
            painter.rect_stroke(screen, 1.5, stroke, StrokeKind::Middle);
            painter.line_segment([center + vec2(-3.5, 7.5), center + vec2(3.5, 7.5)], stroke);
            painter.line_segment([center + vec2(0.0, 4.5), center + vec2(0.0, 7.5)], stroke);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    screen.center() + vec2(-2.0, -3.0),
                    screen.center() + vec2(3.5, 0.0),
                    screen.center() + vec2(-2.0, 3.0),
                ],
                color,
                Stroke::NONE,
            ));
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
        ToolbarIcon::CollapseUp => {
            painter.line_segment([center + vec2(-5.0, 2.5), center + vec2(0.0, -2.5)], stroke);
            painter.line_segment([center + vec2(0.0, -2.5), center + vec2(5.0, 2.5)], stroke);
        }
        ToolbarIcon::Close => {
            painter.line_segment([center + vec2(-4.5, -4.5), center + vec2(4.5, 4.5)], stroke);
            painter.line_segment([center + vec2(4.5, -4.5), center + vec2(-4.5, 4.5)], stroke);
        }
    }
}

fn png_from_rgb(width: usize, height: usize, rgb: Vec<u8>) -> Result<Vec<u8>> {
    let image = image::RgbImage::from_raw(width as u32, height as u32, rgb)
        .context("renderer returned an invalid image")?;
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image).write_to(&mut bytes, image::ImageFormat::Png)?;
    Ok(bytes.into_inner())
}

const BASE_INTERFACE_REGION_SCALE: f32 = 0.6;
/// Every interface size setting is rendered this much larger. The original
/// 100% read too small on typical laptop screens; this keeps saved settings
/// and their percentages while enlarging text and controls uniformly.
const INTERFACE_ZOOM_BOOST: f32 = 1.15;

fn path_breadcrumbs(ui: &mut egui::Ui, path: Option<&Path>) -> Option<BreadcrumbAction> {
    let scale = toolbar_visual_scale(ui);
    let Some(path) = path else {
        ui.label(
            RichText::new("No document open")
                .size(13.0 * scale)
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
                        .size(13.0 * scale)
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
                    .size(13.0 * scale)
                    .color(Color32::from_rgb(92, 92, 92)),
            );
        }
    }
    action
}

fn interface_zoom(interface_scale: f32) -> f32 {
    (interface_scale * BASE_INTERFACE_REGION_SCALE * INTERFACE_ZOOM_BOOST).clamp(0.28, 5.0)
}

fn interface_region_scale(region_scale: f32) -> f32 {
    (region_scale / BASE_INTERFACE_REGION_SCALE).clamp(0.1, 4.0)
}

fn normalized_scale_percent(value: f64, baseline: f32) -> i32 {
    (value / f64::from(baseline) * 100.0).round() as i32
}

fn presentation_fit_zoom(page_size: [f32; 2], viewport: egui::Vec2) -> f32 {
    let fit_width = (viewport.x - 8.0).max(1.0) / page_size[0].max(1.0);
    let fit_height = (viewport.y - 8.0).max(1.0) / page_size[1].max(1.0);
    fit_width.min(fit_height).clamp(0.2, 8.0)
}

fn centered_page_rect(container: Rect, display_size: egui::Vec2) -> Rect {
    Rect::from_center_size(container.center(), display_size)
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

/// Turn Ctrl/Command + a conventional mouse-wheel step into document zoom.
/// Point-granularity events are intentionally left to `trackpad_pinch_factor`
/// so precision touchpad gestures are never applied twice.
fn ctrl_mouse_wheel_zoom_factor(events: &[egui::Event]) -> f32 {
    let wheel_lines = events
        .iter()
        .filter_map(|event| match event {
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta,
                modifiers,
                ..
            } if modifiers.ctrl || modifiers.command => Some(delta.y),
            _ => None,
        })
        .sum::<f32>();

    if wheel_lines.abs() > f32::EPSILON {
        (wheel_lines * 0.12).exp().clamp(0.5, 2.0)
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
            if id == "io.snapdf.SnaPdf.desktop" || !seen.insert(id.to_owned()) {
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
    fn dot_cli_argument_resolves_to_the_current_directory_project() {
        let expected = std::env::current_dir().unwrap().canonicalize().unwrap();
        assert_eq!(
            launch_targets_from_args([std::ffi::OsString::from(".")]),
            vec![LaunchTarget::Project(expected)]
        );
    }

    #[test]
    fn bounded_panel_content_does_not_restore_a_wider_intrinsic_size() {
        egui::__run_test_ui(|ui| {
            let response = egui::Panel::left("bounded-panel-test")
                .resizable(true)
                .default_size(42.0)
                .min_size(0.0)
                .show(ui, |ui| {
                    bounded_panel_content(ui, "bounded-panel-test-content", |ui| {
                        ui.label("This deliberately long label must not widen the side panel.");
                    });
                });
            assert!(
                response.response.rect.width() <= 42.0,
                "panel expanded to {} despite its 42px requested width",
                response.response.rect.width()
            );
        });
    }

    #[test]
    fn chat_messages_stay_inside_a_narrow_panel() {
        egui::__run_test_ui(|ui| {
            ui.set_width(180.0);
            let message = ChatMessage {
                role: ChatRole::Assistant,
                text: "A deliberately long response that must wrap instead of disappearing beyond either edge of the study-chat panel.".to_owned(),
                attachments: Vec::new(),
            };
            let response = ui.scope(|ui| {
                ui.set_width(180.0);
                chat_message(ui, &message, false, 14.5);
            });
            assert!(
                response.response.rect.width() <= 180.0,
                "chat message expanded to {} points",
                response.response.rect.width()
            );
        });
    }

    #[test]
    fn user_chat_bubbles_are_responsive_and_bounded() {
        for available in [1.0_f32, 80.0, 430.0, 1_200.0] {
            let width = chat_user_bubble_width("A short prompt", available, 16.0);
            assert!(width >= 1.0);
            assert!(width <= available.max(1.0));
            assert!(width <= 680.0);
        }
        assert!(
            chat_user_bubble_width("Short", 430.0, 16.0)
                < chat_user_bubble_width(&"long ".repeat(40), 430.0, 16.0)
        );
    }

    #[test]
    fn chat_titles_keep_meaningful_text_without_forced_dots() {
        assert_eq!(
            normalized_chat_title("  What   is all this — I have no context?  "),
            Some("What is all this — I have no context?".to_owned())
        );
        assert_eq!(normalized_chat_title(" \n\t "), None);
        assert!(
            !normalized_chat_title(&"question ".repeat(80))
                .unwrap()
                .ends_with("...")
        );

        let mut legacy = new_study_chat_session(7);
        legacy.title = "What is all this — I have no c...".to_owned();
        legacy.messages.insert(
            ChatProvider::ChatGpt,
            vec![ChatMessage {
                role: ChatRole::User,
                text: "What is all this — I have no context for this section?".to_owned(),
                attachments: Vec::new(),
            }],
        );
        restore_legacy_chat_title(&mut legacy);
        assert_eq!(
            legacy.title,
            "What is all this — I have no context for this section?"
        );
    }

    #[test]
    fn chat_footer_controls_fit_the_available_row() {
        for available in [180.0_f32, 300.0, 430.0, 900.0] {
            let (model, thinking, spacer) = chat_model_control_widths(available, true);
            assert!(model + thinking + spacer + 6.0 <= available + 0.01);
        }
    }

    #[test]
    fn short_user_chat_message_does_not_fill_the_scroll_height() {
        egui::__run_test_ui(|ui| {
            ui.set_width(360.0);
            let message = ChatMessage {
                role: ChatRole::User,
                text: "A short prompt".to_owned(),
                attachments: Vec::new(),
            };
            let response = ui.scope(|ui| {
                ui.set_width(360.0);
                chat_message(ui, &message, false, 14.5);
            });
            assert!(
                response.response.rect.height() < 120.0,
                "short prompt unexpectedly occupied {} points",
                response.response.rect.height()
            );
        });
    }

    #[test]
    fn chat_composer_grows_from_one_to_four_rows() {
        assert_eq!(chat_prompt_rows("", 320.0), 1);
        assert_eq!(chat_prompt_rows("one line", 320.0), 1);
        assert_eq!(chat_prompt_rows("one\ntwo\nthree", 320.0), 3);
        assert_eq!(chat_prompt_rows(&"long ".repeat(300), 120.0), 4);
        assert!(chat_prompt_visual_rows(&"long ".repeat(300), 120.0) > 4);
        assert_eq!(chat_prompt_viewport_height(1), 22.0);
        assert_eq!(chat_prompt_viewport_height(4), 76.0);
        assert_eq!(chat_prompt_viewport_height(400), 76.0);
        assert!(chat_prompt_content_height(8) > chat_prompt_viewport_height(4));
    }

    #[test]
    fn completed_ready_state_is_not_rendered_as_message_status() {
        assert!(!chat_status_visible("Ready", false));
        assert!(!chat_status_visible("ChatGPT is ready.", false));
        assert!(chat_status_visible("ChatGPT is thinking…", true));
    }

    #[test]
    fn chat_markdown_removes_control_markers_but_keeps_content() {
        egui::__run_test_ui(|ui| {
            let job = markdown_inline_job(
                ui,
                "**Bold** and *italic* with `code` and [a link](https://example.com)",
                13.0,
                CHAT_TEXT,
                false,
            );
            assert_eq!(job.text, "Bold and italic with code and a link");
            assert!(job.sections.len() >= 4);
        });
    }

    #[test]
    fn printing_reports_failures_instead_of_claiming_success() {
        let dialog = PrintDialog {
            printers: Vec::new(),
            printer: "snapdf-test-no-such-printer".to_owned(),
            page_range: String::new(),
            copies: 1,
        };
        let message = send_to_printer(Path::new("Cargo.toml"), &dialog);
        assert!(
            message.starts_with("Printing failed") || message.starts_with("Could not start"),
            "{message}"
        );
    }

    #[test]
    fn boxed_answers_are_unwrapped_for_the_tinted_card() {
        assert_eq!(strip_boxed("x^2"), None);
        assert_eq!(
            strip_boxed("\\boxed{y'=y,\\qquad z'=z}").as_deref(),
            Some("y'=y,\\qquad z'=z")
        );
        assert_eq!(
            strip_boxed("x'=a=\\boxed{\\gamma(x-vt)}.").as_deref(),
            Some("x'=a=\\gamma(x-vt).")
        );
        assert_eq!(
            strip_boxed("\\boxed{\\frac{1}{2}}").as_deref(),
            Some("\\frac{1}{2}")
        );
    }

    #[test]
    fn chat_math_parser_recognizes_inline_and_display_latex() {
        let segments = chat_text_segments(
            r"Distance \(D=a\chi\), then \[v=\frac{dD}{dt}\] at the present time.",
        );
        assert!(segments.contains(&ChatTextSegment::Math(r"D=a\chi", false)));
        assert!(segments.contains(&ChatTextSegment::Math(r"v=\frac{dD}{dt}", true)));

        let block_lines = [r"\[", r"v=\frac{dD}{dt}", r"\]"];
        assert_eq!(
            display_math_block(&block_lines, 0),
            Some((r"v=\frac{dD}{dt}".to_owned(), 3))
        );
    }

    #[test]
    fn chat_display_text_preserves_unicode_and_drops_invalid_controls() {
        assert_eq!(
            normalize_chat_display_text("A → B ⇔ C ← D\u{0007}"),
            "A → B ⇔ C ← D "
        );
    }

    #[test]
    fn screenshot_equations_render_to_real_math_images() {
        let font = chat_math_font().expect("embedded STIX Two Math font");
        for formula in [
            r"D(t)=a(t)\chi",
            r"v=\frac{dD}{dt}=\dot a(t)\chi",
            r"v=\dot a\frac{D}{a}=\underbrace{\frac{\dot a}{a}}_{H(t)}D",
        ] {
            let mut options = PngOptions::new();
            options.font_size_pt = Dim::from_i64(15);
            options.dpi = Dim::from_i64(144);
            options.display = true;
            let png = latex_to_png(formula, font, &options).expect("equation should render");
            assert!(png.starts_with(b"\x89PNG"));
        }
    }

    #[test]
    fn markdown_tables_are_parsed_as_rows_instead_of_plain_pipe_text() {
        let lines = [
            r"| Scale factor \(a\) | Distance \(D=a\chi\) |",
            "|---|---|",
            "| 1 | 3 |",
            "| 2 | 6 |",
        ];
        let table = markdown_table(&lines, 0).expect("valid Markdown table");
        assert_eq!(table.headers.len(), 2);
        assert_eq!(table.rows, vec![vec!["1", "3"], vec!["2", "6"]]);
        assert_eq!(table.consumed, 4);
    }

    #[test]
    fn markdown_table_grid_stays_inside_the_chat_panel() {
        egui::__run_test_ui(|ui| {
            ui.set_width(360.0);
            let table = MarkdownTable {
                headers: vec![
                    r"Scale factor \(a\)".to_owned(),
                    r"Distance \(D=a\chi\)".to_owned(),
                ],
                rows: vec![vec!["1".to_owned(), "3".to_owned()]],
                consumed: 3,
            };
            let response = ui.scope(|ui| {
                ui.set_width(360.0);
                render_markdown_table(ui, &table, 16.0);
            });
            assert!(
                response.response.rect.width() <= 360.0,
                "table expanded to {} points",
                response.response.rect.width()
            );
        });
    }

    #[test]
    fn presentation_fit_keeps_portrait_and_landscape_pages_fully_visible() {
        let viewport = vec2(1200.0, 800.0);
        for page in [[600.0, 1000.0], [1600.0, 700.0]] {
            let zoom = presentation_fit_zoom(page, viewport);
            assert!(page[0] * zoom <= viewport.x);
            assert!(page[1] * zoom <= viewport.y);
        }
    }

    #[test]
    fn presentation_page_is_centered_on_both_axes() {
        let viewport = Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0));
        let page = centered_page_rect(viewport, vec2(480.0, 720.0));
        assert_eq!(page.center(), viewport.center());
    }

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
    fn select_all_spans_every_nonempty_pdf_page() {
        let mut layers = HashMap::new();
        layers.insert(0, Vec::new());
        layers.insert(
            1,
            vec![TextGlyph {
                text: "A".to_owned(),
                bounds: None,
            }],
        );
        layers.insert(
            2,
            vec![
                TextGlyph {
                    text: "B".to_owned(),
                    bounds: None,
                },
                TextGlyph {
                    text: "C".to_owned(),
                    bounds: None,
                },
            ],
        );
        layers.insert(3, Vec::new());

        assert_eq!(
            full_document_text_selection(&layers, 4),
            Some(TextSelection {
                anchor: TextPosition { page: 1, glyph: 0 },
                focus: TextPosition { page: 2, glyph: 1 },
            })
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
    fn ctrl_mouse_wheel_zooms_without_double_counting_precision_pinch() {
        let ctrl = Modifiers {
            ctrl: true,
            ..Default::default()
        };
        let zoom_in = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0.0, 3.0),
            phase: egui::TouchPhase::Move,
            modifiers: ctrl,
        };
        let zoom_out = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0.0, -3.0),
            phase: egui::TouchPhase::Move,
            modifiers: ctrl,
        };
        let precision_pinch = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, 20.0),
            phase: egui::TouchPhase::Move,
            modifiers: ctrl,
        };

        assert!(ctrl_mouse_wheel_zoom_factor(&[zoom_in]) > 1.0);
        assert!(ctrl_mouse_wheel_zoom_factor(&[zoom_out]) < 1.0);
        assert_eq!(ctrl_mouse_wheel_zoom_factor(&[precision_pinch]), 1.0);
    }

    #[test]
    fn interface_regions_keep_independent_effective_scales() {
        let overall = interface_zoom(1.0);
        let boost = INTERFACE_ZOOM_BOOST;
        assert!((overall - BASE_INTERFACE_REGION_SCALE * boost).abs() < 0.0001);
        assert!((overall * interface_region_scale(0.4) - 0.4 * boost).abs() < 0.0001);
        assert!((overall * interface_region_scale(0.8) - 0.8 * boost).abs() < 0.0001);
        assert!((interface_zoom(1.25) - 0.75 * boost).abs() < 0.0001);
        assert_eq!(normalized_scale_percent(1.6, DEFAULT_UI_SCALE), 100);
        assert_eq!(normalized_scale_percent(3.2, DEFAULT_UI_SCALE), 200);
        assert_eq!(
            normalized_scale_percent(0.9, DEFAULT_CHROME_SCALE_RATIO),
            100
        );
        assert_eq!(
            normalized_scale_percent(1.0, DEFAULT_SIDEBAR_SCALE_RATIO),
            100
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn wayland_session_prefers_x11_when_xwayland_is_available() {
        use std::ffi::OsStr;

        assert!(should_prefer_x11_gestures(
            Some(OsStr::new("wayland")),
            Some(OsStr::new("wayland-0")),
            Some(OsStr::new(":0")),
            None,
        ));
        assert!(!should_prefer_x11_gestures(
            Some(OsStr::new("wayland")),
            Some(OsStr::new("wayland-0")),
            None,
            None,
        ));
        assert!(!should_prefer_x11_gestures(
            Some(OsStr::new("wayland")),
            Some(OsStr::new("wayland-0")),
            Some(OsStr::new(":0")),
            Some(OsStr::new("wayland")),
        ));
        assert!(!should_prefer_x11_gestures(
            Some(OsStr::new("x11")),
            None,
            Some(OsStr::new(":0")),
            None,
        ));
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
