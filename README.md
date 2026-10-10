# SnapPDF

SnapPDF is a fast PDF reader for Ubuntu. Keep course material, research papers, and work documents in project folders, read them in tabs, and pick up exactly where you left off.

## What you can do

- Add folders as projects. SnapPDF shows only PDFs, including PDFs in nested directories.
- Browse a familiar file tree, keep a stable Recent list, and give projects or files display names without renaming anything on disk.
- Open several documents in tabs. `Ctrl+Shift+T` restores the last closed tab.
- Select PDF text, copy it with `Ctrl+C`, follow web links, and use contents-page links and document outlines to move around a PDF.
- Search the current document, all open tabs, or every PDF in your workspace and project folders.
- Scroll smoothly, use the arrow keys to move through pages, pinch to zoom the document, and pan enlarged pages with a two-finger slide.
- Highlight text, draw, add comments, shapes, text boxes, stamps, and file references. Right-click an annotation to edit or remove it.
- Export annotations to a copy, or explicitly save supported annotations into the original PDF after SnapPDF creates a backup.
- Print, open the current file with another installed app, and make SnapPDF the default PDF application on Ubuntu.
- Capture a region for local text recognition. Math OCR is marked experimental.

## Install on Ubuntu

From the SnapPDF folder, run:

```bash
./scripts/install.sh
```

SnapPDF is then available from the Applications menu, can be pinned to the dock, and is registered as the default handler for PDF files. Your projects, recent documents, display names, and annotation overlays stay on this computer in `~/.config/snappdf/state.json`.

## Reading modes

- **Hide toolbar** keeps the document and sidebar visible.
- **Reader mode** hides the toolbar and sidebar. Use the small floating control to return.
- **Presentation mode** enters full screen and shows only the document. Press `Esc` to leave.

In presentation mode, move between pages with `Space` / `Backspace`, `Right` / `Left`, or `J` / `K`.

## Shortcuts

| Shortcut | Action |
|---|---|
| `Ctrl+O` | Open a PDF |
| `Ctrl+K`, then `Ctrl+O` | Add a project folder |
| `Ctrl+F` | Open search |
| `Ctrl+P` | Print |
| `Ctrl+W` | Close current tab |
| `Ctrl+Shift+T` | Restore the most recently closed tab |
| `Ctrl++` / `Ctrl+-` | Scale the interface and document |
| `Up` / `Down` | Scroll the document |
| `Left` / `Right` | Previous / next page |
| Drag text, then `Ctrl+C` | Select and copy text |
| `Ctrl+Z` / `Ctrl+Shift+Z` | Undo / redo an annotation |

## Build from source

```bash
cargo run --release
```

Or open a document directly:

```bash
cargo run --release -- /absolute/path/to/document.pdf
```
