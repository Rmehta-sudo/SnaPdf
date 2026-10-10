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
- Global and per-project recent documents that persist across launches.
- Open-from-file-manager support and Ubuntu `application/pdf` integration.
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

## State and privacy

Projects and recent documents are stored locally in `~/.config/crisppdf/state.json`. CrispPDF does
not upload documents or telemetry.

## Planned follow-up features

1. PDF text layer with native selection, copy, and in-document search.
2. Printing and page-range controls.
3. Highlights, comments, annotations, and safe incremental PDF saves.
4. Region screenshot tool with a right-side OCR/extracted-text panel.
5. Tabs, restored reading positions, bookmarks, and document outlines.

These features are intentionally outside the initial release so the rendering, navigation, and
project model can stabilize first.
