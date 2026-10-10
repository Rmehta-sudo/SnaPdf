# CrispPDF

CrispPDF is a fast, project-oriented PDF viewer for Ubuntu, written in Rust. It combines a
Codex-style project sidebar with a crisp, continuously scrolling PDF canvas.

## Features

- Native Rust desktop app using `egui`/`eframe`.
- Bundled PDFium renderer; no system PDF library is required.
- Background page rendering so input and scrolling stay responsive.
- High-DPI page textures, virtualized rendering, and a bounded texture cache.
- Whole-interface zoom (`Ctrl++`, `Ctrl+-`, `Ctrl+0`), fit-to-width, continuous scrolling,
  restored reading position, and drag-and-drop.
- Project folders with recursive PDF-only discovery and filename filtering.
- Stable alphabetical file order inside projects; opening a PDF never reshuffles the project tree.
- The 15 most recently opened PDFs, ordered once at startup so clicking never reshuffles the UI.
- Display-only project and PDF renaming from each sidebar item's right-click menu.
- Selectable PDF text with drag selection and `Ctrl+C` copy.
- Clickable safe web/email links and internal PDF page links, including generated fallbacks for
  contents pages that only expose bookmarks and visible text.
- PDF links stay interactive regardless of the currently selected annotation tool.
- Arrow-key navigation: up/down scroll; left/right move to the previous/next page, with accelerated
  repeat while an arrow is held.
- Faster animated wheel scrolling with strong acceleration while up/down arrows are held.
- Hover-only three-dot menus with polished project/PDF actions.
- Open-from-file-manager support and Ubuntu `application/pdf` integration.
- Stable Ubuntu app/dock identity with a dedicated CrispPDF icon.
- Obsidian-inspired dark UI with a narrow icon rail, nested file tree, document tabs, selectable
  copyable path bar, quiet surfaces, and restrained hover/selection states.
- Vector-drawn sidebar icons, so the UI does not depend on missing Unicode glyphs.
- Library and Contents sidebar tabs; Contents is generated from the PDF's bookmark outline.
- Persistent, non-destructive highlights, freehand drawing/signatures, comments, text boxes,
  rectangles/ellipses, stamps, attachment markers, color selection, erasing, and undo/redo.
- Comments always render above highlights. Right-click highlights to add/edit/delete comments;
  right-click comments and other annotations to edit or delete them. The eraser supports direct
  click-and-drag removal, and selecting an active tool again returns to Select mode.
- Multiple persistent document tabs, in-document search with page highlights, CUPS printing with
  printer/page-range/copy controls, and export of PDF-native highlights, comments, text boxes,
  shapes, and stamps into a new PDF copy.
- An in-app **Open with...** chooser lists other installed PDF-capable applications for any sidebar
  PDF or the currently open document.
- Snip OCR with an editable result popup and explicit Copy button; nothing is copied automatically.
- Exact embedded-text extraction first, then fully local PP-OCRv5 image OCR when needed.
- A separate Math OCR tool powered by PP-FormulaNet-S that converts formula images to LaTeX.
- Conventional top-right close controls on rename, comment, and OCR dialogs.

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
files, makes it the default PDF viewer, and installs the CPU-only local OCR environment. No `sudo`
is required. OCR models download once and remain in `~/.local/share/crisppdf/ocr/models`.

## Shortcuts

| Shortcut | Action |
|---|---|
| `Ctrl+O` | Open PDF |
| `Ctrl++` | Scale the entire app up |
| `Ctrl+-` | Scale the entire app down |
| `Ctrl+0` | Reset the entire app to 100% |
| `Ctrl+F` | Find text in the current PDF |
| `Ctrl+P` | Open the print dialog |
| `Up` / `Down` | Scroll document |
| `Left` / `Right` | Previous / next page |
| Drag text, `Ctrl+C` | Select and copy PDF text |
| `Ctrl+Z` / `Ctrl+Shift+Z` | Undo / redo an annotation |
| Click a PDF link | Open a safe web/email link or jump within the document |

## State and privacy

Projects, recent documents, display names, and annotation overlays are stored locally in
`~/.config/crisppdf/state.json`. OCR runs locally after the one-time model download. CrispPDF does
not upload documents or telemetry, and annotation tools do not modify the original PDF.

## Export note

Highlights, comments, text boxes, shapes, and stamps can be exported into a new annotated PDF.
Freehand ink/signatures and attachment markers remain in CrispPDF's local overlay store in this
release; the original PDF is never modified.
