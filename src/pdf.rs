use crossbeam_channel::{Receiver, Sender, unbounded};
use pdfium_bundled::pdfium_render::prelude::*;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Width of the quick first pass drawn before a page's sharp render, so a
/// page never sits blank while a large render is still queued.
pub const PREVIEW_RENDER_WIDTH: u16 = 480;

#[derive(Debug)]
pub enum RenderCommand {
    Open {
        path: PathBuf,
        generation: u64,
        /// For encrypted PDFs; `None` tries without one.
        password: Option<String>,
    },
    RenderPage {
        page_index: usize,
        target_width: u16,
        generation: u64,
    },
    ExtractText {
        page_index: usize,
        generation: u64,
    },
    Search {
        query: String,
        generation: u64,
        request_id: u64,
    },
    ExportAnnotated {
        source: PathBuf,
        password: Option<String>,
        destination: PathBuf,
        annotations: Vec<Annotation>,
        generation: u64,
    },
    RenderRegion {
        page_index: usize,
        /// PDF coordinates: left, bottom, right, top.
        bounds: [f32; 4],
        generation: u64,
        request_id: u64,
    },
    Shutdown,
}

#[derive(Debug, Clone)]
pub struct TextGlyph {
    pub text: String,
    /// PDF coordinates: left, bottom, right, top.
    pub bounds: Option<[f32; 4]>,
}

#[derive(Debug, Clone)]
pub enum LinkTarget {
    Uri(String),
    Page(usize),
}

#[derive(Debug, Clone)]
pub struct PdfLinkRegion {
    /// PDF coordinates: left, bottom, right, top.
    pub bounds: [f32; 4],
    pub target: LinkTarget,
}

#[derive(Debug, Clone)]
pub struct PdfOutlineItem {
    pub title: String,
    pub depth: usize,
    pub target: Option<LinkTarget>,
}

#[derive(Debug, Clone)]
pub struct PdfSearchMatch {
    pub page_index: usize,
    pub excerpt: String,
}

#[derive(Debug, Clone)]
pub struct PdfFileSearchMatches {
    pub path: PathBuf,
    pub matches: Vec<PdfSearchMatch>,
}

/// Coalesces character-level text bounds into the line-level quadrilaterals
/// expected by native PDF highlight annotations. Exporting one quadrilateral
/// per glyph makes conforming viewers draw a rounded highlight capsule around
/// every character, which appears as a row of vertical/hourglass marks.
fn merge_highlight_rects(rects: &[[f32; 4]]) -> Vec<[f32; 4]> {
    let mut merged = Vec::new();

    for rect in rects {
        if !rect.iter().all(|value| value.is_finite()) {
            continue;
        }
        let rect = [
            rect[0].min(rect[2]),
            rect[1].min(rect[3]),
            rect[0].max(rect[2]),
            rect[1].max(rect[3]),
        ];
        let width = rect[2] - rect[0];
        let height = rect[3] - rect[1];
        if width <= f32::EPSILON || height <= f32::EPSILON {
            continue;
        }

        if let Some(line) = merged.last_mut() {
            let line: &mut [f32; 4] = line;
            let line_height = line[3] - line[1];
            let overlap = line[3].min(rect[3]) - line[1].max(rect[1]);
            let same_line = overlap >= line_height.min(height) * 0.5;
            let horizontal_gap = if rect[0] > line[2] {
                rect[0] - line[2]
            } else if line[0] > rect[2] {
                line[0] - rect[2]
            } else {
                0.0
            };
            let close_enough = horizontal_gap <= line_height.max(height) * 1.5;

            if same_line && close_enough {
                line[0] = line[0].min(rect[0]);
                line[1] = line[1].min(rect[1]);
                line[2] = line[2].max(rect[2]);
                line[3] = line[3].max(rect[3]);
                continue;
            }
        }

        merged.push(rect);
    }

    merged
}

fn highlight_quad_points(rect: [f32; 4]) -> PdfQuadPoints {
    // PDF text-markup annotations use a special Z-order rather than the
    // counter-clockwise order used by general quadrilaterals: the first edge
    // is the top of the text and the second edge is the bottom.
    PdfQuadPoints::new_from_values(
        rect[0], rect[3], rect[2], rect[3], rect[0], rect[1], rect[2], rect[1],
    )
}

/// Search PDFs independently of the viewer's rendering engine.
///
/// PDFium is process-global and can only be initialised once, so a second PDFium
/// worker silently fails after the viewer has started. `pdftotext` is already
/// provided by Poppler and gives this background scan its own parser.
pub fn search_pdf_files(
    paths: &[PathBuf],
    query: &str,
    max_matches_per_file: usize,
) -> Result<Vec<PdfFileSearchMatches>, String> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Ok(Vec::new());
    }

    let mut results = Vec::new();
    for path in paths {
        let Ok(output) = Command::new("pdftotext")
            .args(["-layout", "-enc", "UTF-8"])
            .arg(path)
            .arg("-")
            .output()
        else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut matches = Vec::new();
        for (page_index, original) in text.split('\u{c}').enumerate() {
            let lower = original.to_lowercase();
            if !lower.contains(&needle) {
                continue;
            }
            let excerpt = original
                .lines()
                .find(|line| line.to_lowercase().contains(&needle))
                .unwrap_or(original)
                .split_whitespace()
                .take(22)
                .collect::<Vec<_>>()
                .join(" ");
            for _ in lower.match_indices(&needle) {
                matches.push(PdfSearchMatch {
                    page_index,
                    excerpt: excerpt.clone(),
                });
                if matches.len() >= max_matches_per_file {
                    break;
                }
            }
            if matches.len() >= max_matches_per_file {
                break;
            }
        }
        if !matches.is_empty() {
            results.push(PdfFileSearchMatches {
                path: path.clone(),
                matches,
            });
        }
    }
    Ok(results)
}

#[derive(Debug)]
pub enum RenderEvent {
    Ready,
    DocumentOpened {
        path: PathBuf,
        generation: u64,
        page_sizes: Vec<[f32; 2]>,
        outline: Vec<PdfOutlineItem>,
    },
    PageRendered {
        page_index: usize,
        generation: u64,
        target_width: u16,
        width: usize,
        height: usize,
        rgba: Vec<u8>,
    },
    /// The PDF is encrypted and needs a (different) password to open.
    PasswordRequired {
        path: PathBuf,
        generation: u64,
        /// A password was supplied and rejected.
        incorrect: bool,
    },
    /// A render request was dropped because the reader had already moved on.
    /// The UI must forget it so the page is requested again if revisited.
    RenderSkipped {
        page_index: usize,
        generation: u64,
        target_width: u16,
    },
    TextExtracted {
        page_index: usize,
        generation: u64,
        glyphs: Vec<TextGlyph>,
        links: Vec<PdfLinkRegion>,
    },
    SearchCompleted {
        generation: u64,
        request_id: u64,
        query: String,
        matches: Vec<PdfSearchMatch>,
    },
    ExportCompleted {
        generation: u64,
        destination: PathBuf,
        exported: usize,
        skipped: usize,
    },
    RegionRendered {
        generation: u64,
        request_id: u64,
        width: usize,
        height: usize,
        rgb: Vec<u8>,
    },
    Error {
        generation: u64,
        message: String,
    },
}

pub struct RenderWorker {
    pub commands: Sender<RenderCommand>,
    pub events: Receiver<RenderEvent>,
    focused_page: Arc<AtomicUsize>,
    desired_generation: Arc<AtomicU64>,
}

impl RenderWorker {
    pub fn spawn(repaint: eframe::egui::Context) -> Self {
        let (command_tx, command_rx) = unbounded();
        let (event_tx, event_rx) = unbounded();
        let focused_page = Arc::new(AtomicUsize::new(0));
        let desired_generation = Arc::new(AtomicU64::new(0));
        let worker_focus = focused_page.clone();
        let worker_generation = desired_generation.clone();

        std::thread::Builder::new()
            .name("snapdf-renderer".to_owned())
            .spawn(move || {
                worker_loop(
                    command_rx,
                    event_tx,
                    repaint,
                    worker_focus,
                    worker_generation,
                )
            })
            .expect("could not start PDF renderer thread");

        Self {
            commands: command_tx,
            events: event_rx,
            focused_page,
            desired_generation,
        }
    }

    pub fn focus_page(&self, page: usize) {
        self.focused_page.store(page, Ordering::Relaxed);
    }

    pub fn expect_generation(&self, generation: u64) {
        self.desired_generation.store(generation, Ordering::Relaxed);
    }
}

/// Pick the most useful queued command. Document-level commands keep their
/// arrival order; page work goes to whatever is nearest the page being read,
/// with its quick preview first, then its sharp render, then its text layer.
/// Fast paging therefore never waits behind renders for pages already left.
fn next_command(queue: &mut VecDeque<RenderCommand>, focus: usize) -> Option<RenderCommand> {
    let page_priority = |command: &RenderCommand| match command {
        RenderCommand::RenderPage {
            page_index,
            target_width,
            ..
        } => Some((
            page_index.abs_diff(focus),
            u8::from(*target_width > PREVIEW_RENDER_WIDTH),
        )),
        RenderCommand::ExtractText { page_index, .. } => Some((page_index.abs_diff(focus), 2)),
        _ => None,
    };
    let index = queue
        .iter()
        .position(|command| page_priority(command).is_none())
        .or_else(|| {
            queue
                .iter()
                .enumerate()
                .min_by_key(|(index, command)| (page_priority(command), *index))
                .map(|(index, _)| index)
        })?;
    queue.remove(index)
}

/// Add a freehand stroke or closed outline as a drawing annotation that
/// other viewers can show, select and delete, instead of burning lines into
/// the page content. It is a Stamp carrying the path as its appearance:
/// pdfium-render cannot write the /InkList that MuPDF and Poppler require to
/// accept an Ink annotation, while every viewer renders a Stamp's appearance.
fn add_drawing_annotation(
    document: &PdfDocument,
    page: &mut PdfPage,
    points: &[[f32; 2]],
    closed: bool,
    color: [u8; 4],
    width: f32,
) -> Result<(), PdfiumError> {
    let Some(first) = points.first() else {
        return Ok(());
    };
    let color = PdfColor::new(color[0], color[1], color[2], color[3]);
    let mut path = PdfPagePathObject::new(
        document,
        PdfPoints::new(first[0]),
        PdfPoints::new(first[1]),
        Some(color),
        Some(PdfPoints::new(width)),
        None,
    )?;
    for point in &points[1..] {
        path.line_to(PdfPoints::new(point[0]), PdfPoints::new(point[1]))?;
    }
    if closed {
        path.close_path()?;
    }
    path.set_line_cap(PdfPageObjectLineCap::Round)?;
    path.set_line_join(PdfPageObjectLineJoin::Round)?;
    let pad = width.max(1.0);
    let left = points.iter().map(|point| point[0]).fold(f32::MAX, f32::min) - pad;
    let right = points.iter().map(|point| point[0]).fold(f32::MIN, f32::max) + pad;
    let bottom = points.iter().map(|point| point[1]).fold(f32::MAX, f32::min) - pad;
    let top = points.iter().map(|point| point[1]).fold(f32::MIN, f32::max) + pad;
    let mut drawing = page.annotations_mut().create_stamp_annotation()?;
    // Bounds first: PDFium sizes the appearance's /BBox from the annotation
    // rectangle when the object is attached. Attached first, it stays empty
    // and other viewers clip the drawing away.
    drawing.set_bounds(PdfRect::new_from_values(bottom, left, top, right))?;
    drawing.objects_mut().add_path_object(path)?;
    // The colour lives on the path object; pdfium-render 0.9.4 crashes when
    // set_stroke_color is called on an annotation that owns objects.
    drawing.set_creator("SnaPdf")?;
    drawing.set_is_printed(true)?;
    Ok(())
}

/// A closed polyline approximating the ellipse inscribed in `rect`
/// (left, bottom, right, top).
fn ellipse_outline(rect: [f32; 4]) -> Vec<[f32; 2]> {
    let center = [(rect[0] + rect[2]) / 2.0, (rect[1] + rect[3]) / 2.0];
    let radius = [
        (rect[2] - rect[0]).abs() / 2.0,
        (rect[3] - rect[1]).abs() / 2.0,
    ];
    (0..64)
        .map(|step| {
            let angle = step as f32 / 64.0 * std::f32::consts::TAU;
            [
                center[0] + radius[0] * angle.cos(),
                center[1] + radius[1] * angle.sin(),
            ]
        })
        .collect()
}

/// Write `annotations` into a copy of `source` at `destination` as standard
/// PDF annotations. Returns how many were exported and how many skipped.
fn export_annotated(
    pdfium: &Pdfium,
    source: &std::path::Path,
    password: Option<&str>,
    destination: &std::path::Path,
    annotations: &[Annotation],
) -> Result<(usize, usize), String> {
    let mut exported_document = pdfium
        .load_pdf_from_file(&source, password)
        .map_err(|error| error.to_string())?;
    let mut exported = 0;
    let mut skipped = 0;
    for annotation in annotations {
        let mut page = exported_document
            .pages_mut()
            .get(annotation.page as i32)
            .map_err(|error| error.to_string())?;
        match &annotation.kind {
            AnnotationKind::Highlight { rects, color } => {
                let rects = merge_highlight_rects(rects);
                if rects.is_empty() {
                    skipped += 1;
                    continue;
                }
                let mut highlight = page
                    .annotations_mut()
                    .create_highlight_annotation()
                    .map_err(|error| error.to_string())?;
                let left = rects.iter().map(|rect| rect[0]).fold(f32::MAX, f32::min);
                let bottom = rects.iter().map(|rect| rect[1]).fold(f32::MAX, f32::min);
                let right = rects.iter().map(|rect| rect[2]).fold(f32::MIN, f32::max);
                let top = rects.iter().map(|rect| rect[3]).fold(f32::MIN, f32::max);
                highlight
                    .set_bounds(PdfRect::new_from_values(bottom, left, top, right))
                    .map_err(|error| error.to_string())?;
                highlight
                    .set_stroke_color(PdfColor::new(color[0], color[1], color[2], color[3]))
                    .map_err(|error| error.to_string())?;
                for rect in &rects {
                    highlight
                        .attachment_points_mut()
                        .create_attachment_point_at_end(highlight_quad_points(*rect))
                        .map_err(|error| error.to_string())?;
                }
                if let Some(comment) =
                    annotations
                        .iter()
                        .find_map(|candidate| match &candidate.kind {
                            AnnotationKind::Note {
                                text,
                                parent: Some(parent),
                                ..
                            } if *parent == annotation.id => Some(text.as_str()),
                            _ => None,
                        })
                {
                    highlight
                        .set_contents(comment)
                        .map_err(|error| error.to_string())?;
                }
                highlight
                    .set_creator("SnaPdf")
                    .map_err(|error| error.to_string())?;
                highlight
                    .set_is_printed(true)
                    .map_err(|error| error.to_string())?;
                exported += 1;
            }
            AnnotationKind::Note {
                position,
                text,
                color,
                parent,
            } => {
                if parent.is_some() {
                    continue;
                }
                let mut note = page
                    .annotations_mut()
                    .create_text_annotation(text)
                    .map_err(|error| error.to_string())?;
                note.set_bounds(PdfRect::new_from_values(
                    position[1] - 9.0,
                    position[0] - 9.0,
                    position[1] + 9.0,
                    position[0] + 9.0,
                ))
                .map_err(|error| error.to_string())?;
                note.set_stroke_color(PdfColor::new(color[0], color[1], color[2], color[3]))
                    .map_err(|error| error.to_string())?;
                note.set_creator("SnaPdf")
                    .map_err(|error| error.to_string())?;
                note.set_is_printed(true)
                    .map_err(|error| error.to_string())?;
                exported += 1;
            }
            AnnotationKind::Ink {
                points,
                color,
                width,
            } => {
                if points.len() < 2 {
                    skipped += 1;
                    continue;
                }
                add_drawing_annotation(
                    &exported_document,
                    &mut page,
                    points,
                    false,
                    *color,
                    *width,
                )
                .map_err(|error| error.to_string())?;
                exported += 1;
            }
            AnnotationKind::TextBox { rect, text, color } => {
                let mut text_box = page
                    .annotations_mut()
                    .create_free_text_annotation(text)
                    .map_err(|error| error.to_string())?;
                text_box
                    .set_bounds(PdfRect::new_from_values(rect[1], rect[0], rect[3], rect[2]))
                    .map_err(|error| error.to_string())?;
                // For FreeText, /C is the box *background*; setting the text
                // colour there produced solid coloured boxes in other viewers.
                let _ = color;
                text_box
                    .set_is_printed(true)
                    .map_err(|error| error.to_string())?;
                exported += 1;
            }
            AnnotationKind::Shape {
                rect,
                color,
                width,
                ellipse: true,
            } => {
                add_drawing_annotation(
                    &exported_document,
                    &mut page,
                    &ellipse_outline(*rect),
                    true,
                    *color,
                    *width,
                )
                .map_err(|error| error.to_string())?;
                exported += 1;
            }
            AnnotationKind::Shape {
                rect,
                color,
                ellipse: false,
                ..
            } => {
                let bounds = PdfRect::new_from_values(rect[1], rect[0], rect[3], rect[2]);
                let mut shape = page
                    .annotations_mut()
                    .create_square_annotation()
                    .map_err(|error| error.to_string())?;
                shape
                    .set_bounds(bounds)
                    .map_err(|error| error.to_string())?;
                shape
                    .set_contents("Rectangle")
                    .map_err(|error| error.to_string())?;
                shape
                    .set_stroke_color(PdfColor::new(color[0], color[1], color[2], color[3]))
                    .map_err(|error| error.to_string())?;
                shape
                    .set_is_printed(true)
                    .map_err(|error| error.to_string())?;
                exported += 1;
            }
            AnnotationKind::Stamp {
                position,
                text,
                color,
            } => {
                let mut stamp = page
                    .annotations_mut()
                    .create_free_text_annotation(&text.to_uppercase())
                    .map_err(|error| error.to_string())?;
                stamp
                    .set_bounds(PdfRect::new_from_values(
                        position[1] - 28.0,
                        position[0],
                        position[1],
                        position[0] + (text.len() as f32 * 8.0 + 16.0).max(72.0),
                    ))
                    .map_err(|error| error.to_string())?;
                let _ = color;
                stamp
                    .set_is_printed(true)
                    .map_err(|error| error.to_string())?;
                exported += 1;
            }
            AnnotationKind::Attachment { position, path } => {
                let mut note = page
                    .annotations_mut()
                    .create_text_annotation(&format!("Attachment reference: {}", path.display()))
                    .map_err(|error| error.to_string())?;
                note.set_bounds(PdfRect::new_from_values(
                    position[1] - 9.0,
                    position[0] - 9.0,
                    position[1] + 9.0,
                    position[0] + 9.0,
                ))
                .map_err(|error| error.to_string())?;
                note.set_creator("SnaPdf")
                    .map_err(|error| error.to_string())?;
                note.set_is_printed(true)
                    .map_err(|error| error.to_string())?;
                exported += 1;
            }
        }
    }
    exported_document
        .save_to_file(&destination)
        .map_err(|error| error.to_string())?;
    Ok((exported, skipped))
}

fn worker_loop(
    commands: Receiver<RenderCommand>,
    events: Sender<RenderEvent>,
    repaint: eframe::egui::Context,
    focused_page: Arc<AtomicUsize>,
    desired_generation: Arc<AtomicU64>,
) {
    let pdfium = match pdfium_bundled::bind_bundled() {
        Ok(pdfium) => pdfium,
        Err(error) => {
            let _ = events.send(RenderEvent::Error {
                generation: 0,
                message: format!("Could not initialize PDFium: {error}"),
            });
            repaint.request_repaint();
            return;
        }
    };

    let _ = events.send(RenderEvent::Ready);
    repaint.request_repaint();

    let mut document = None;
    let mut active_generation = 0;

    let mut queue = VecDeque::new();
    loop {
        if queue.is_empty() {
            match commands.recv() {
                Ok(command) => queue.push_back(command),
                Err(_) => break,
            }
        }
        queue.extend(commands.try_iter());
        let Some(command) = next_command(&mut queue, focused_page.load(Ordering::Relaxed)) else {
            continue;
        };
        match command {
            RenderCommand::Open {
                path,
                generation,
                password,
            } => {
                active_generation = generation;
                match pdfium.load_pdf_from_file(&path, password.as_deref()) {
                    Ok(opened) => {
                        let page_sizes = opened
                            .pages()
                            .iter()
                            .map(|page| [page.width().value, page.height().value])
                            .collect();
                        let mut outline = Vec::new();
                        append_bookmarks(opened.bookmarks().root(), 0, &mut outline);
                        document = Some(opened);
                        let _ = events.send(RenderEvent::DocumentOpened {
                            path,
                            generation,
                            page_sizes,
                            outline,
                        });
                    }
                    Err(PdfiumError::PdfiumLibraryInternalError(
                        PdfiumInternalError::PasswordError,
                    )) => {
                        document = None;
                        let _ = events.send(RenderEvent::PasswordRequired {
                            path,
                            generation,
                            incorrect: password.is_some(),
                        });
                    }
                    Err(error) => {
                        document = None;
                        let _ = events.send(RenderEvent::Error {
                            generation,
                            message: format!("Could not open PDF: {error}"),
                        });
                    }
                }
                repaint.request_repaint();
            }
            RenderCommand::RenderPage {
                page_index,
                target_width,
                generation,
            } => {
                let focus = focused_page.load(Ordering::Relaxed);
                let Some(document) = document.as_ref().filter(|_| {
                    generation == active_generation
                        && generation == desired_generation.load(Ordering::Relaxed)
                        && page_index.abs_diff(focus) <= 2
                }) else {
                    let _ = events.send(RenderEvent::RenderSkipped {
                        page_index,
                        generation,
                        target_width,
                    });
                    repaint.request_repaint();
                    continue;
                };

                let rendered = (|| -> Result<_, PdfiumError> {
                    let page = document.pages().get(page_index as i32)?;
                    let bitmap = page.render_with_config(
                        &PdfRenderConfig::new()
                            .set_target_width(target_width as i32)
                            .render_annotations(true),
                    )?;
                    let image = bitmap.as_image()?.to_rgba8();
                    Ok((
                        image.width() as usize,
                        image.height() as usize,
                        image.into_raw(),
                    ))
                })();

                match rendered {
                    Ok((width, height, rgba)) => {
                        let _ = events.send(RenderEvent::PageRendered {
                            page_index,
                            generation,
                            target_width,
                            width,
                            height,
                            rgba,
                        });
                    }
                    Err(error) => {
                        let _ = events.send(RenderEvent::Error {
                            generation,
                            message: format!("Could not render page {}: {error}", page_index + 1),
                        });
                    }
                }
                repaint.request_repaint();
            }
            RenderCommand::ExtractText {
                page_index,
                generation,
            } => {
                // Rendering is intentionally restricted to the focused pages,
                // but text extraction also serves a selection spanning several
                // pages. Do not drop those requests just because the user has
                // scrolled away from an intermediate page.
                if generation != active_generation
                    || generation != desired_generation.load(Ordering::Relaxed)
                {
                    continue;
                }
                let Some(document) = document.as_ref() else {
                    continue;
                };

                let extracted =
                    (|| -> Result<(Vec<TextGlyph>, Vec<PdfLinkRegion>), PdfiumError> {
                        let page = document.pages().get(page_index as i32)?;
                        let text = page.text()?;
                        let glyphs = text
                            .chars()
                            .iter()
                            .filter_map(|character| {
                                let text = character.unicode_string()?;
                                let bounds = character.loose_bounds().ok().map(|rect| {
                                    [
                                        rect.left().value,
                                        rect.bottom().value,
                                        rect.right().value,
                                        rect.top().value,
                                    ]
                                });
                                Some(TextGlyph { text, bounds })
                            })
                            .collect();
                        let links = page
                            .links()
                            .iter()
                            .filter_map(|link| {
                                let rect = link.rect().ok()?;
                                let target = link
                                    .action()
                                    .and_then(|action| {
                                        if let Some(uri_action) = action.as_uri_action() {
                                            uri_action.uri().ok().map(LinkTarget::Uri)
                                        } else if let Some(local_action) =
                                            action.as_local_destination_action()
                                        {
                                            local_action
                                                .destination()
                                                .ok()?
                                                .page_index()
                                                .ok()
                                                .map(|page| LinkTarget::Page(page as usize))
                                        } else {
                                            None
                                        }
                                    })
                                    .or_else(|| {
                                        link.destination()?
                                            .page_index()
                                            .ok()
                                            .map(|page| LinkTarget::Page(page as usize))
                                    })?;
                                Some(PdfLinkRegion {
                                    bounds: [
                                        rect.left().value,
                                        rect.bottom().value,
                                        rect.right().value,
                                        rect.top().value,
                                    ],
                                    target,
                                })
                            })
                            .collect();
                        Ok((glyphs, links))
                    })();

                match extracted {
                    Ok((glyphs, links)) => {
                        let _ = events.send(RenderEvent::TextExtracted {
                            page_index,
                            generation,
                            glyphs,
                            links,
                        });
                    }
                    Err(error) => {
                        let _ = events.send(RenderEvent::Error {
                            generation,
                            message: format!(
                                "Could not extract text from page {}: {error}",
                                page_index + 1
                            ),
                        });
                    }
                }
                repaint.request_repaint();
            }
            RenderCommand::Search {
                query,
                generation,
                request_id,
            } => {
                if generation != active_generation
                    || generation != desired_generation.load(Ordering::Relaxed)
                {
                    continue;
                }
                let Some(document) = document.as_ref() else {
                    continue;
                };
                let needle = query.to_lowercase();
                let mut matches = Vec::new();
                if !needle.trim().is_empty() {
                    for (page_index, page) in document.pages().iter().enumerate() {
                        let Ok(text) = page.text() else {
                            continue;
                        };
                        let original = text.all();
                        let lower = original.to_lowercase();
                        let excerpt = original
                            .lines()
                            .find(|line| line.to_lowercase().contains(&needle))
                            .unwrap_or(&original)
                            .split_whitespace()
                            .take(22)
                            .collect::<Vec<_>>()
                            .join(" ");
                        for _ in lower.match_indices(&needle) {
                            matches.push(PdfSearchMatch {
                                page_index,
                                excerpt: excerpt.clone(),
                            });
                            if matches.len() >= 500 {
                                break;
                            }
                        }
                        if matches.len() >= 500 {
                            break;
                        }
                    }
                }
                let _ = events.send(RenderEvent::SearchCompleted {
                    generation,
                    request_id,
                    query,
                    matches,
                });
                repaint.request_repaint();
            }
            RenderCommand::ExportAnnotated {
                source,
                password,
                destination,
                annotations,
                generation,
            } => {
                if generation != active_generation {
                    continue;
                }
                let result = export_annotated(
                    &pdfium,
                    &source,
                    password.as_deref(),
                    &destination,
                    &annotations,
                );
                match result {
                    Ok((exported, skipped)) => {
                        let _ = events.send(RenderEvent::ExportCompleted {
                            generation,
                            destination,
                            exported,
                            skipped,
                        });
                    }
                    Err(message) => {
                        let _ = events.send(RenderEvent::Error {
                            generation,
                            message: format!("Could not export annotated PDF: {message}"),
                        });
                    }
                }
                repaint.request_repaint();
            }
            RenderCommand::RenderRegion {
                page_index,
                bounds,
                generation,
                request_id,
            } => {
                if generation != active_generation
                    || generation != desired_generation.load(Ordering::Relaxed)
                {
                    continue;
                }
                let Some(document) = document.as_ref() else {
                    continue;
                };
                let rendered = (|| -> Result<_, PdfiumError> {
                    let page = document.pages().get(page_index as i32)?;
                    let page_width = page.width().value.max(1.0);
                    let page_height = page.height().value.max(1.0);
                    let bitmap = page.render_with_config(
                        &PdfRenderConfig::new()
                            .set_target_width(2400)
                            .render_annotations(true),
                    )?;
                    let image = bitmap.as_image()?.to_rgb8();
                    let [left, bottom, right, top] = bounds;
                    let x1 = (left.min(right) / page_width * image.width() as f32)
                        .floor()
                        .clamp(0.0, image.width().saturating_sub(1) as f32)
                        as u32;
                    let x2 = (left.max(right) / page_width * image.width() as f32)
                        .ceil()
                        .clamp((x1 + 1) as f32, image.width() as f32)
                        as u32;
                    let y1 = ((page_height - top.max(bottom)) / page_height * image.height() as f32)
                        .floor()
                        .clamp(0.0, image.height().saturating_sub(1) as f32)
                        as u32;
                    let y2 = ((page_height - top.min(bottom)) / page_height * image.height() as f32)
                        .ceil()
                        .clamp((y1 + 1) as f32, image.height() as f32)
                        as u32;
                    let crop =
                        image::imageops::crop_imm(&image, x1, y1, x2 - x1, y2 - y1).to_image();
                    Ok((
                        crop.width() as usize,
                        crop.height() as usize,
                        crop.into_raw(),
                    ))
                })();
                match rendered {
                    Ok((width, height, rgb)) => {
                        let _ = events.send(RenderEvent::RegionRendered {
                            generation,
                            request_id,
                            width,
                            height,
                            rgb,
                        });
                    }
                    Err(error) => {
                        let _ = events.send(RenderEvent::Error {
                            generation,
                            message: format!("Could not render OCR region: {error}"),
                        });
                    }
                }
                repaint.request_repaint();
            }
            RenderCommand::Shutdown => break,
        }
    }
}

fn append_bookmarks(
    mut bookmark: Option<PdfBookmark<'_>>,
    depth: usize,
    output: &mut Vec<PdfOutlineItem>,
) {
    while let Some(item) = bookmark {
        let next = item.next_sibling();
        let target = item
            .destination()
            .and_then(|destination| destination.page_index().ok())
            .map(|page| LinkTarget::Page(page as usize))
            .or_else(|| {
                item.action().and_then(|action| {
                    if let Some(uri_action) = action.as_uri_action() {
                        uri_action.uri().ok().map(LinkTarget::Uri)
                    } else if let Some(local_action) = action.as_local_destination_action() {
                        local_action
                            .destination()
                            .ok()?
                            .page_index()
                            .ok()
                            .map(|page| LinkTarget::Page(page as usize))
                    } else {
                        None
                    }
                })
            });
        if let Some(title) = item.title().filter(|title| !title.trim().is_empty()) {
            output.push(PdfOutlineItem {
                title,
                depth: depth.min(8),
                target,
            });
        }
        append_bookmarks(item.first_child(), depth + 1, output);
        bookmark = next;
    }
}
use crate::model::{Annotation, AnnotationKind};

#[cfg(test)]
mod tests {
    use super::{
        PREVIEW_RENDER_WIDTH, RenderCommand, highlight_quad_points, merge_highlight_rects,
        next_command,
    };
    use std::collections::VecDeque;

    fn render(page_index: usize, target_width: u16) -> RenderCommand {
        RenderCommand::RenderPage {
            page_index,
            target_width,
            generation: 1,
        }
    }

    fn order(queue: &mut VecDeque<RenderCommand>, focus: usize) -> Vec<(usize, u16)> {
        std::iter::from_fn(|| next_command(queue, focus))
            .map(|command| match command {
                RenderCommand::RenderPage {
                    page_index,
                    target_width,
                    ..
                } => (page_index, target_width),
                RenderCommand::ExtractText { page_index, .. } => (page_index, 0),
                _ => (usize::MAX, 0),
            })
            .collect()
    }

    #[test]
    fn focused_page_preview_jumps_ahead_of_older_queued_renders() {
        let mut queue = VecDeque::from([
            render(3, 2400),
            RenderCommand::ExtractText {
                page_index: 5,
                generation: 1,
            },
            render(5, 2400),
            render(4, PREVIEW_RENDER_WIDTH),
            render(5, PREVIEW_RENDER_WIDTH),
        ]);
        assert_eq!(
            order(&mut queue, 5),
            vec![
                (5, PREVIEW_RENDER_WIDTH),
                (5, 2400),
                (5, 0),
                (4, PREVIEW_RENDER_WIDTH),
                (3, 2400),
            ]
        );
    }

    #[test]
    fn document_commands_run_before_page_work_in_arrival_order() {
        let mut queue = VecDeque::from([
            render(0, 2400),
            RenderCommand::Shutdown,
            render(0, PREVIEW_RENDER_WIDTH),
        ]);
        assert!(matches!(
            next_command(&mut queue, 0),
            Some(RenderCommand::Shutdown)
        ));
    }

    #[test]
    fn character_highlights_are_merged_into_visual_lines() {
        let rects = [
            [10.0, 90.0, 18.0, 100.0],
            [18.5, 90.0, 27.0, 100.0],
            [31.0, 89.5, 40.0, 100.5],
            [10.0, 70.0, 19.0, 80.0],
            [19.5, 70.0, 28.0, 80.0],
        ];

        assert_eq!(
            merge_highlight_rects(&rects),
            vec![[10.0, 89.5, 40.0, 100.5], [10.0, 70.0, 28.0, 80.0]]
        );
    }

    #[test]
    fn distant_columns_remain_separate_highlight_runs() {
        let rects = [
            [10.0, 90.0, 20.0, 100.0],
            [21.0, 90.0, 30.0, 100.0],
            [100.0, 90.0, 110.0, 100.0],
        ];

        assert_eq!(
            merge_highlight_rects(&rects),
            vec![[10.0, 90.0, 30.0, 100.0], [100.0, 90.0, 110.0, 100.0]]
        );
    }

    #[test]
    fn text_markup_quad_points_use_pdf_z_order() {
        let points = highlight_quad_points([10.0, 20.0, 30.0, 40.0]);
        assert_eq!((points.x1().value, points.y1().value), (10.0, 40.0));
        assert_eq!((points.x2().value, points.y2().value), (30.0, 40.0));
        assert_eq!((points.x3().value, points.y3().value), (10.0, 20.0));
        assert_eq!((points.x4().value, points.y4().value), (30.0, 20.0));
    }

    /// Export one annotation of every kind and read the file back with
    /// PDFium: each must be a real PDF annotation (never page content), so
    /// other viewers can show, edit, and delete it.
    #[test]
    fn exported_annotations_are_standard_pdf_annotations() {
        use super::export_annotated;
        use crate::model::{Annotation, AnnotationKind};
        use pdfium_bundled::pdfium_render::prelude::*;

        let directory = std::env::temp_dir().join(format!("snapdf-export-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let source = directory.join("source.pdf");
        let destination = directory.join("annotated.pdf");
        let pdfium = pdfium_bundled::bind_bundled().unwrap();
        {
            let mut document = pdfium.create_new_pdf().unwrap();
            document
                .pages_mut()
                .create_page_at_end(PdfPagePaperSize::a4())
                .unwrap();
            document.save_to_file(&source).unwrap();
        }
        let color = [220, 40, 40, 255];
        let kinds = vec![
            AnnotationKind::Highlight {
                rects: vec![[72.0, 700.0, 300.0, 714.0]],
                color: [255, 211, 74, 255],
            },
            AnnotationKind::Ink {
                points: vec![
                    [100.0, 600.0],
                    [140.0, 640.0],
                    [180.0, 600.0],
                    [220.0, 640.0],
                ],
                color,
                width: 3.0,
            },
            AnnotationKind::Note {
                position: [400.0, 700.0],
                text: "A comment".to_owned(),
                color: [255, 211, 74, 255],
                parent: None,
            },
            AnnotationKind::TextBox {
                rect: [72.0, 480.0, 300.0, 520.0],
                text: "Text box".to_owned(),
                color,
            },
            AnnotationKind::Shape {
                rect: [72.0, 360.0, 220.0, 440.0],
                color,
                width: 2.0,
                ellipse: false,
            },
            AnnotationKind::Shape {
                rect: [260.0, 360.0, 420.0, 440.0],
                color,
                width: 2.0,
                ellipse: true,
            },
            AnnotationKind::Stamp {
                position: [72.0, 300.0],
                text: "Approved".to_owned(),
                color,
            },
        ];
        let annotations: Vec<_> = kinds
            .into_iter()
            .enumerate()
            .map(|(id, kind)| Annotation {
                id: id as u64 + 1,
                page: 0,
                kind,
            })
            .collect();

        let (exported, skipped) =
            export_annotated(&pdfium, &source, None, &destination, &annotations).unwrap();
        assert_eq!((exported, skipped), (7, 0));

        let document = pdfium.load_pdf_from_file(&destination, None).unwrap();
        let page = document.pages().get(0).unwrap();
        let mut types: Vec<_> = page
            .annotations()
            .iter()
            .map(|annotation| format!("{:?}", annotation.annotation_type()))
            .collect();
        types.sort();
        assert_eq!(
            types,
            [
                "FreeText",
                "FreeText",
                "Highlight",
                "Square",
                "Stamp",
                "Stamp",
                "Text"
            ]
        );
        assert_eq!(
            page.objects().len(),
            0,
            "nothing may be drawn into page content"
        );
        println!("{}", destination.display());
    }
}
