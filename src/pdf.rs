use crossbeam_channel::{Receiver, Sender, unbounded};
use pdfium_bundled::pdfium_render::prelude::*;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

#[derive(Debug)]
pub enum RenderCommand {
    Open {
        path: PathBuf,
        generation: u64,
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

    while let Ok(command) = commands.recv() {
        match command {
            RenderCommand::Open { path, generation } => {
                active_generation = generation;
                match pdfium.load_pdf_from_file(&path, None) {
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
                if generation != active_generation
                    || generation != desired_generation.load(Ordering::Relaxed)
                    || page_index.abs_diff(focus) > 2
                {
                    continue;
                }
                let Some(document) = document.as_ref() else {
                    continue;
                };

                let rendered = (|| -> Result<_, PdfiumError> {
                    let page = document.pages().get(page_index as i32)?;
                    let bitmap = page.render_with_config(
                        &PdfRenderConfig::new().set_target_width(target_width as i32),
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
                destination,
                annotations,
                generation,
            } => {
                if generation != active_generation {
                    continue;
                }
                let result = (|| -> Result<(usize, usize), String> {
                    let mut exported_document = pdfium
                        .load_pdf_from_file(&source, None)
                        .map_err(|error| error.to_string())?;
                    let mut exported = 0;
                    let mut skipped = 0;
                    for annotation in &annotations {
                        let mut page = exported_document
                            .pages_mut()
                            .get(annotation.page as i32)
                            .map_err(|error| error.to_string())?;
                        match &annotation.kind {
                            AnnotationKind::Highlight { rects, color } => {
                                if rects.is_empty() {
                                    skipped += 1;
                                    continue;
                                }
                                let mut highlight = page
                                    .annotations_mut()
                                    .create_highlight_annotation()
                                    .map_err(|error| error.to_string())?;
                                let left =
                                    rects.iter().map(|rect| rect[0]).fold(f32::MAX, f32::min);
                                let bottom =
                                    rects.iter().map(|rect| rect[1]).fold(f32::MAX, f32::min);
                                let right =
                                    rects.iter().map(|rect| rect[2]).fold(f32::MIN, f32::max);
                                let top = rects.iter().map(|rect| rect[3]).fold(f32::MIN, f32::max);
                                highlight
                                    .set_bounds(PdfRect::new_from_values(bottom, left, top, right))
                                    .map_err(|error| error.to_string())?;
                                highlight
                                    .set_stroke_color(PdfColor::new(
                                        color[0], color[1], color[2], color[3],
                                    ))
                                    .map_err(|error| error.to_string())?;
                                for rect in rects {
                                    highlight
                                        .attachment_points_mut()
                                        .create_attachment_point_at_end(
                                            PdfQuadPoints::new_from_values(
                                                rect[0], rect[1], rect[2], rect[1], rect[2],
                                                rect[3], rect[0], rect[3],
                                            ),
                                        )
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
                                note.set_stroke_color(PdfColor::new(
                                    color[0], color[1], color[2], color[3],
                                ))
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
                                for segment in points.windows(2) {
                                    page.objects_mut()
                                        .create_path_object_line(
                                            PdfPoints::new(segment[0][0]),
                                            PdfPoints::new(segment[0][1]),
                                            PdfPoints::new(segment[1][0]),
                                            PdfPoints::new(segment[1][1]),
                                            PdfColor::new(color[0], color[1], color[2], color[3]),
                                            PdfPoints::new(*width),
                                        )
                                        .map_err(|error| error.to_string())?;
                                }
                                exported += 1;
                            }
                            AnnotationKind::TextBox { rect, text, color } => {
                                let mut text_box = page
                                    .annotations_mut()
                                    .create_free_text_annotation(text)
                                    .map_err(|error| error.to_string())?;
                                text_box
                                    .set_bounds(PdfRect::new_from_values(
                                        rect[1], rect[0], rect[3], rect[2],
                                    ))
                                    .map_err(|error| error.to_string())?;
                                text_box
                                    .set_stroke_color(PdfColor::new(
                                        color[0], color[1], color[2], color[3],
                                    ))
                                    .map_err(|error| error.to_string())?;
                                text_box
                                    .set_is_printed(true)
                                    .map_err(|error| error.to_string())?;
                                exported += 1;
                            }
                            AnnotationKind::Shape {
                                rect,
                                color,
                                ellipse,
                                ..
                            } => {
                                let bounds =
                                    PdfRect::new_from_values(rect[1], rect[0], rect[3], rect[2]);
                                let mut shape = page
                                    .annotations_mut()
                                    .create_square_annotation()
                                    .map_err(|error| error.to_string())?;
                                shape
                                    .set_bounds(bounds)
                                    .map_err(|error| error.to_string())?;
                                shape
                                    .set_contents(if *ellipse { "Ellipse" } else { "Rectangle" })
                                    .map_err(|error| error.to_string())?;
                                shape
                                    .set_stroke_color(PdfColor::new(
                                        color[0], color[1], color[2], color[3],
                                    ))
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
                                stamp
                                    .set_stroke_color(PdfColor::new(
                                        color[0], color[1], color[2], color[3],
                                    ))
                                    .map_err(|error| error.to_string())?;
                                stamp
                                    .set_is_printed(true)
                                    .map_err(|error| error.to_string())?;
                                exported += 1;
                            }
                            AnnotationKind::Attachment { position, path } => {
                                let mut note = page
                                    .annotations_mut()
                                    .create_text_annotation(&format!(
                                        "Attachment reference: {}",
                                        path.display()
                                    ))
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
                })();
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
                    let bitmap =
                        page.render_with_config(&PdfRenderConfig::new().set_target_width(2400))?;
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
