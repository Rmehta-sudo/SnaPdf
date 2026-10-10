use crate::model::{AppState, is_pdf};
use crate::pdf::{RenderCommand, RenderEvent, RenderWorker};
use crate::{storage, theme};
use anyhow::{Result, anyhow};
use eframe::egui::{
    self, Align, Align2, Color32, ColorImage, CornerRadius, FontId, Frame, Key, KeyboardShortcut,
    Layout, Margin, Modifiers, Rect, Response, RichText, ScrollArea, Sense, Stroke, StrokeKind,
    TextureHandle, TextureOptions, pos2, vec2,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const MAX_TEXTURES: usize = 10;
const MAX_TEXTURE_BYTES: usize = 256 * 1024 * 1024;
const MAX_PENDING_PAGES: usize = 6;
const PAGE_GAP: f32 = 18.0;

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
            .with_title("CrispPDF")
            .with_inner_size([1360.0, 860.0])
            .with_min_inner_size([820.0, 560.0]),
        ..Default::default()
    };

    eframe::run_native(
        "CrispPDF",
        options,
        Box::new(move |creation| Ok(Box::new(CrispPdfApp::new(creation, initial_path)))),
    )
    .map_err(|error| anyhow!(error.to_string()))
}

pub struct CrispPdfApp {
    state: AppState,
    worker: RenderWorker,
    project_files: HashMap<PathBuf, Vec<PathBuf>>,
    current_path: Option<PathBuf>,
    page_sizes: Vec<[f32; 2]>,
    textures: HashMap<usize, CachedPage>,
    pending_pages: HashSet<PageKey>,
    generation: u64,
    frame_number: u64,
    zoom: f32,
    current_page: usize,
    sidebar_search: String,
    status: String,
    renderer_ready: bool,
    fit_width_requested: bool,
}

impl CrispPdfApp {
    fn new(creation: &eframe::CreationContext<'_>, initial_path: Option<PathBuf>) -> Self {
        theme::apply(&creation.egui_ctx);
        let mut state = storage::load();
        state.prune_missing();
        let project_files = state
            .projects
            .iter()
            .map(|project| (project.root.clone(), project.pdf_files()))
            .collect();

        let mut app = Self {
            state,
            worker: RenderWorker::spawn(creation.egui_ctx.clone()),
            project_files,
            current_path: None,
            page_sizes: Vec::new(),
            textures: HashMap::new(),
            pending_pages: HashSet::new(),
            generation: 0,
            frame_number: 0,
            zoom: 1.0,
            current_page: 0,
            sidebar_search: String::new(),
            status: "Preparing PDF renderer...".to_owned(),
            renderer_ready: false,
            fit_width_requested: true,
        };

        if let Some(path) = initial_path {
            app.open_document(path);
        }
        app
    }

    fn open_document(&mut self, path: PathBuf) {
        if !path.is_file() || !is_pdf(&path) {
            self.status = "CrispPDF can only open PDF files.".to_owned();
            return;
        }

        self.generation = self.generation.wrapping_add(1);
        self.worker.expect_generation(self.generation);
        self.current_path = Some(path.clone());
        self.page_sizes.clear();
        self.textures.clear();
        self.pending_pages.clear();
        self.current_page = 0;
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

    fn refresh_projects(&mut self) {
        self.project_files = self
            .state
            .projects
            .iter()
            .map(|project| (project.root.clone(), project.pdf_files()))
            .collect();
        self.status = "Project folders refreshed.".to_owned();
    }

    fn persist(&mut self) {
        if let Err(error) = storage::save(&self.state) {
            self.status = format!("Could not save app state: {error}");
        }
    }

    fn handle_events(&mut self, ctx: &egui::Context) {
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
                } if generation == self.generation => {
                    self.page_sizes = page_sizes;
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
                RenderEvent::Error {
                    generation,
                    message,
                } if generation == 0 || generation == self.generation => {
                    self.status = message;
                }
                _ => {}
            }
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

    fn keyboard_shortcuts(&mut self, ctx: &egui::Context) {
        let open = KeyboardShortcut::new(Modifiers::CTRL, Key::O);
        let zoom_in = KeyboardShortcut::new(Modifiers::CTRL, Key::Plus);
        let zoom_out = KeyboardShortcut::new(Modifiers::CTRL, Key::Minus);
        let fit = KeyboardShortcut::new(Modifiers::CTRL, Key::Num0);

        if ctx.input_mut(|input| input.consume_shortcut(&open)) {
            self.open_file_dialog();
        }
        if ctx.input_mut(|input| input.consume_shortcut(&zoom_in)) {
            self.zoom = (self.zoom + 0.1).min(3.0);
        }
        if ctx.input_mut(|input| input.consume_shortcut(&zoom_out)) {
            self.zoom = (self.zoom - 0.1).max(0.4);
        }
        if ctx.input_mut(|input| input.consume_shortcut(&fit)) {
            self.fit_width_requested = true;
        }
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
        egui::Panel::left("project-sidebar")
            .resizable(true)
            .default_size(300.0)
            .min_size(248.0)
            .max_size(440.0)
            .frame(
                Frame::new()
                    .fill(theme::SIDEBAR_BG)
                    .inner_margin(Margin::same(10)),
            )
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("CrispPDF").size(20.0).strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .button("Add folder")
                            .on_hover_text("Add a project folder")
                            .clicked()
                        {
                            self.add_project_dialog();
                        }
                    });
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .button("Open")
                        .on_hover_text("Open a PDF (Ctrl+O)")
                        .clicked()
                    {
                        self.open_file_dialog();
                    }
                    if ui
                        .button("Refresh")
                        .on_hover_text("Rescan project folders")
                        .clicked()
                    {
                        self.refresh_projects();
                    }
                });
                ui.add_space(6.0);
                ui.add_sized(
                    [ui.available_width(), 30.0],
                    egui::TextEdit::singleline(&mut self.sidebar_search)
                        .hint_text("Search PDFs")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(10.0);

                ScrollArea::vertical()
                    .id_salt("sidebar-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.label(section_label("PROJECTS"));
                        let mut remove_project = None;
                        let mut open_after_ui = None;

                        if self.state.projects.is_empty() {
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(
                                    "Add a course or research folder. Only PDFs are shown.",
                                )
                                .color(theme::MUTED)
                                .size(13.0),
                            );
                        }

                        for index in 0..self.state.projects.len() {
                            let project = self.state.projects[index].clone();
                            let files = self
                                .project_files
                                .get(&project.root)
                                .cloned()
                                .unwrap_or_default();
                            let matching_files: Vec<_> = files
                                .into_iter()
                                .filter(|path| {
                                    matches_search(path, &project.root, &self.sidebar_search)
                                })
                                .collect();

                            ui.add_space(3.0);
                            let header = project_header(
                                ui,
                                &project.name,
                                matching_files.len(),
                                self.state.projects[index].expanded,
                            )
                            .on_hover_text(project.root.display().to_string());
                            if header.clicked() {
                                self.state.projects[index].expanded =
                                    !self.state.projects[index].expanded;
                                self.persist();
                            }
                            header.context_menu(|ui| {
                                if ui.button("Remove project").clicked() {
                                    remove_project = Some(index);
                                    ui.close();
                                }
                            });

                            if self.state.projects[index].expanded {
                                for path in matching_files.iter().take(300) {
                                    let relative = path.strip_prefix(&project.root).unwrap_or(path);
                                    let selected =
                                        self.current_path.as_deref() == Some(path.as_path());
                                    if nav_row(
                                        ui,
                                        &relative.display().to_string(),
                                        selected,
                                        NavIcon::File,
                                        24.0,
                                    )
                                    .on_hover_text(path.display().to_string())
                                    .clicked()
                                    {
                                        open_after_ui = Some(path.clone());
                                    }
                                }
                                if matching_files.len() > 300 {
                                    ui.label(
                                        RichText::new(format!(
                                            "{} more PDFs",
                                            matching_files.len() - 300
                                        ))
                                        .color(theme::MUTED)
                                        .size(12.0),
                                    );
                                }
                            }
                        }

                        ui.add_space(16.0);
                        ui.label(section_label("RECENTS"));
                        let recent = self.state.recent_documents.clone();
                        for item in recent.iter().take(8) {
                            let selected =
                                self.current_path.as_deref() == Some(item.path.as_path());
                            if nav_row(ui, &file_name(&item.path), selected, NavIcon::Clock, 0.0)
                                .on_hover_text(item.path.display().to_string())
                                .clicked()
                            {
                                open_after_ui = Some(item.path.clone());
                            }
                        }

                        if let Some(index) = remove_project {
                            let project = self.state.projects.remove(index);
                            self.project_files.remove(&project.root);
                            self.persist();
                        }
                        if let Some(path) = open_after_ui {
                            self.open_document(path);
                        }
                    });
            });
    }

    fn toolbar(&mut self, root: &mut egui::Ui) {
        egui::Panel::top("toolbar")
            .frame(
                Frame::new()
                    .fill(theme::PANEL_BG)
                    .inner_margin(Margin::symmetric(12, 8)),
            )
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    let title = self
                        .current_path
                        .as_deref()
                        .map(file_name)
                        .unwrap_or_else(|| "No document open".to_owned());
                    ui.label(RichText::new(title).strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(&self.status).color(theme::MUTED).size(12.0));
                        ui.separator();
                        if ui.button("Fit width").clicked() {
                            self.fit_width_requested = true;
                        }
                        if ui.button("+").on_hover_text("Zoom in - Ctrl++").clicked() {
                            self.zoom = (self.zoom + 0.1).min(3.0);
                        }
                        ui.label(format!("{}%", (self.zoom * 100.0).round() as i32));
                        if ui.button("-").on_hover_text("Zoom out - Ctrl+-").clicked() {
                            self.zoom = (self.zoom - 0.1).max(0.4);
                        }
                        if !self.page_sizes.is_empty() {
                            ui.separator();
                            ui.label(format!(
                                "{} / {}",
                                self.current_page + 1,
                                self.page_sizes.len()
                            ));
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

                let available_page_width = (ui.available_width() - 56.0).max(100.0);
                if self.fit_width_requested {
                    let first_width = self.page_sizes[0][0].max(1.0);
                    self.zoom = (available_page_width / first_width).clamp(0.4, 3.0);
                    self.fit_width_requested = false;
                }

                let pixels_per_point = ctx.pixels_per_point();
                let page_sizes = self.page_sizes.clone();
                let generation = self.generation;
                let zoom = self.zoom;
                let frame_number = self.frame_number;
                let mut visible_page = self.current_page;

                ScrollArea::vertical()
                    .id_salt("document-scroll")
                    .auto_shrink([false, false])
                    .show_viewport(ui, |ui, _viewport| {
                        ui.add_space(PAGE_GAP);
                        let mut laid_out_pages = Vec::with_capacity(page_sizes.len());
                        for (index, source_size) in page_sizes.iter().enumerate() {
                            let display_size = vec2(source_size[0] * zoom, source_size[1] * zoom);
                            let row_width = ui.available_width();
                            let (row_rect, _) = ui.allocate_exact_size(
                                vec2(row_width, display_size.y + PAGE_GAP),
                                Sense::hover(),
                            );
                            let page_rect = Rect::from_min_size(
                                pos2(row_rect.center().x - display_size.x / 2.0, row_rect.min.y),
                                display_size,
                            );
                            laid_out_pages.push((index, page_rect));
                        }
                        ui.add_space(PAGE_GAP);

                        let viewport = ui.clip_rect();
                        if let Some((index, _)) =
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

                        let preload = viewport.expand2(vec2(0.0, viewport.height() * 1.25));
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
                            }
                        }
                    });
                self.current_page = visible_page;
            });
    }

    fn empty_state(&self, ui: &mut egui::Ui) {
        ui.centered_and_justified(|ui| {
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("CrispPDF").size(34.0).strong());
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Your PDFs, organized by project.")
                        .size(17.0)
                        .color(theme::MUTED),
                );
                ui.add_space(18.0);
                ui.label("Open a PDF, add a project folder, or drag a PDF into this window.");
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Ctrl+O  |  Ctrl++  |  Ctrl+-  |  Ctrl+0").color(theme::MUTED),
                );
                if !self.renderer_ready {
                    ui.add_space(16.0);
                    ui.spinner();
                    ui.label(&self.status);
                }
            });
        });
    }
}

impl eframe::App for CrispPdfApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame_number = self.frame_number.wrapping_add(1);
        self.handle_events(ctx);
        self.keyboard_shortcuts(ctx);
        self.dropped_files(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.sidebar(ui);
        self.toolbar(ui);
        self.viewer(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = self.worker.commands.send(RenderCommand::Shutdown);
        self.persist();
    }
}

#[derive(Clone, Copy)]
enum NavIcon {
    Clock,
    File,
}

fn section_label(text: &str) -> RichText {
    RichText::new(text).size(11.0).color(theme::MUTED).strong()
}

fn project_header(ui: &mut egui::Ui, name: &str, count: usize, expanded: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(rect, CornerRadius::same(6), theme::PANEL_BG);
    }

    let icon_color = Color32::from_rgb(205, 207, 214);
    let chevron_x = rect.left() + 8.0;
    let center_y = rect.center().y;
    let chevron = if expanded {
        [
            pos2(chevron_x, center_y - 2.0),
            pos2(chevron_x + 4.0, center_y + 2.0),
            pos2(chevron_x + 8.0, center_y - 2.0),
        ]
    } else {
        [
            pos2(chevron_x + 2.0, center_y - 4.0),
            pos2(chevron_x + 6.0, center_y),
            pos2(chevron_x + 2.0, center_y + 4.0),
        ]
    };
    painter.add(egui::Shape::line(
        chevron.to_vec(),
        Stroke::new(1.5, theme::MUTED),
    ));
    paint_folder_icon(painter, pos2(rect.left() + 24.0, center_y), icon_color);

    let count_text = count.to_string();
    painter.text(
        pos2(rect.right() - 8.0, center_y),
        Align2::RIGHT_CENTER,
        &count_text,
        FontId::proportional(11.0),
        theme::MUTED,
    );
    let max_chars = ((rect.width() - 88.0) / 7.2).max(4.0) as usize;
    painter.text(
        pos2(rect.left() + 48.0, center_y),
        Align2::LEFT_CENTER,
        elide(name, max_chars),
        FontId::proportional(14.0),
        icon_color,
    );
    response
}

fn nav_row(ui: &mut egui::Ui, label: &str, selected: bool, icon: NavIcon, indent: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    let painter = ui.painter();
    if selected {
        painter.rect_filled(
            rect.shrink2(vec2(1.0, 1.0)),
            CornerRadius::same(6),
            theme::ACCENT.gamma_multiply(0.42),
        );
    } else if response.hovered() {
        painter.rect_filled(
            rect.shrink2(vec2(1.0, 1.0)),
            CornerRadius::same(6),
            theme::PANEL_BG,
        );
    }

    let icon_color = if selected {
        Color32::WHITE
    } else {
        theme::MUTED
    };
    let icon_center = pos2(rect.left() + indent + 13.0, rect.center().y);
    match icon {
        NavIcon::Clock => paint_clock_icon(painter, icon_center, icon_color),
        NavIcon::File => paint_file_icon(painter, icon_center, icon_color),
    }
    let text_left = rect.left() + indent + 28.0;
    let max_chars = ((rect.right() - text_left - 6.0) / 7.0).max(4.0) as usize;
    painter.text(
        pos2(text_left, rect.center().y),
        Align2::LEFT_CENTER,
        elide(label, max_chars),
        FontId::proportional(13.5),
        if selected {
            Color32::WHITE
        } else {
            Color32::from_rgb(211, 212, 217)
        },
    );
    response
}

fn paint_file_icon(painter: &egui::Painter, center: egui::Pos2, color: Color32) {
    let rect = Rect::from_center_size(center, vec2(10.0, 14.0));
    painter.rect_stroke(rect, 1.0, Stroke::new(1.2, color), StrokeKind::Inside);
    painter.line_segment(
        [
            pos2(rect.left() + 2.0, center.y - 2.0),
            pos2(rect.right() - 2.0, center.y - 2.0),
        ],
        Stroke::new(1.0, color.gamma_multiply(0.8)),
    );
    painter.line_segment(
        [
            pos2(rect.left() + 2.0, center.y + 2.0),
            pos2(rect.right() - 2.0, center.y + 2.0),
        ],
        Stroke::new(1.0, color.gamma_multiply(0.8)),
    );
}

fn paint_clock_icon(painter: &egui::Painter, center: egui::Pos2, color: Color32) {
    painter.circle_stroke(center, 6.0, Stroke::new(1.2, color));
    painter.line_segment(
        [center, pos2(center.x, center.y - 3.5)],
        Stroke::new(1.2, color),
    );
    painter.line_segment(
        [center, pos2(center.x + 2.8, center.y + 1.7)],
        Stroke::new(1.2, color),
    );
}

fn paint_folder_icon(painter: &egui::Painter, center: egui::Pos2, color: Color32) {
    let points = vec![
        pos2(center.x - 7.0, center.y - 4.0),
        pos2(center.x - 1.5, center.y - 4.0),
        pos2(center.x + 0.5, center.y - 2.0),
        pos2(center.x + 7.0, center.y - 2.0),
        pos2(center.x + 7.0, center.y + 5.0),
        pos2(center.x - 7.0, center.y + 5.0),
        pos2(center.x - 7.0, center.y - 4.0),
    ];
    painter.add(egui::Shape::line(points, Stroke::new(1.3, color)));
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
    (((clamped + 127) / 128) * 128).min(2600) as u16
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
