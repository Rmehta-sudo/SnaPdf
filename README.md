# CrispPDF

CrispPDF is a fast, project-oriented PDF viewer for Ubuntu, written in Rust. It combines a
Codex-style project sidebar with a crisp, continuously scrolling PDF canvas.

## Initial release

- Native Rust desktop app using `egui`/`eframe`.
- Bundled PDFium renderer; no system PDF library is required.
- Background page rendering so input and scrolling stay responsive.
- High-DPI page textures, virtualized rendering, and a bounded texture cache.
- Zoom controls, fit-to-width, continuous scrolling, page position, and drag-and-drop.
- Project folders with recursive PDF-only discovery and filename filtering.
- Stable alphabetical file order inside projects; opening a PDF never reshuffles the project tree.
- The 15 most recently opened PDFs, ordered once at startup so clicking never reshuffles the UI.
- Display-only project and PDF renaming from each sidebar item's right-click menu.
- Selectable PDF text with drag selection and `Ctrl+C` copy.
- Clickable safe web/email links and internal PDF page links.
- Arrow-key navigation: up/down scroll; left/right move to the previous/next page.
- Faster animated wheel scrolling with strong acceleration while up/down arrows are held.
- Hover-only three-dot menus with polished project/PDF actions.
- Open-from-file-manager support and Ubuntu `application/pdf` integration.
- Stable Ubuntu app/dock identity with a dedicated CrispPDF icon.
- Dark, compact project UI inspired by Codex.
- Vector-drawn sidebar icons, so the UI does not depend on missing Unicode glyphs.

## Build and run

```bash
cargo run --release
cargo run --release -- /absolute/path/to/document.pdf
```

The first build downloads Rust dependencies and the pinned PDFium binary. PDFium is embedded in
the release executable, so installed copies do not need a runtime download.

## Install on Ubuntu

```bash
./scripts/install.sh
```

This installs into your user account, adds CrispPDF to the application menu, registers it for PDF
files, and makes it the default PDF viewer. No `sudo` is required.

## Shortcuts

| Shortcut | Action |
|---|---|
| `Ctrl+O` | Open PDF |
| `Ctrl++` | Zoom in |
| `Ctrl+-` | Zoom out |
| `Ctrl+0` | Fit width |
| `Up` / `Down` | Scroll document |
| `Left` / `Right` | Previous / next page |
| Drag text, `Ctrl+C` | Select and copy PDF text |
| Click a PDF link | Open a safe web/email link or jump within the document |

## State and privacy

Projects and recent documents are stored locally in `~/.config/crisppdf/state.json`. CrispPDF does
not upload documents or telemetry.

## Planned follow-up features

1. In-document text search.
2. Printing and page-range controls.
3. Highlights, comments, annotations, and safe incremental PDF saves.
4. Region screenshot tool with a right-side OCR/extracted-text panel.
5. Tabs, restored reading positions, bookmarks, and document outlines.

These features are intentionally outside the initial release so the rendering, navigation, and
project model can stabilize first.
