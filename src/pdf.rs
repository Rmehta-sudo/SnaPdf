use crossbeam_channel::{Receiver, Sender, unbounded};
use pdfium_bundled::pdfium_render::prelude::*;
use std::path::PathBuf;
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
            .name("crisppdf-renderer".to_owned())
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
