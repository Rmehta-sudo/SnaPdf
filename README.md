# SnapPDF

## A Project-Based PDF Reading Workspace

Normal PDF viewers are excellent for one document at a time. Managing dozens of lecture notes, research papers, and PDFs spread across unrelated folders is a different problem.

**SnapPDF brings those documents into one organized reading workspace—without moving them.**

- **Multiple projects, one workspace.** Add completely unrelated folders from anywhere on your computer as separate projects, all available from one sidebar.
- **A real PDF folder explorer.** Browse every project through its original nested directory structure while automatically hiding non-PDF files.
- **Tabs, recents, and resume.** Keep several PDFs open, reopen recently closed tabs, and return to remembered projects, annotations, and reading positions between sessions.
- **Search with context.** Search the current document, every open tab, or every nested PDF in the current project.
- **Reading and annotation tools.** Select and copy text, follow links, highlight, comment, draw, add shapes or text, export annotations, and use Reader or Presentation mode.
- **Offline OCR.** Capture a region with Snip OCR, inspect its local result in a scrollable dialog, then choose whether to copy it. Math OCR is available as an experimental option.

**No imports, duplicate files, or cloud storage are required.** Your PDFs stay in their original locations; SnapPDF simply makes them easier to navigate, read, search, and return to.

In short, SnapPDF is not just another PDF viewer. It is a fast, filesystem-first workspace for managing and reading PDFs across multiple projects.

## Install on a new Linux machine

The installer is intended to be the one command you run after cloning the repository. On systems with `apt`, it installs the needed Linux build packages, Rust, and the local OCR runtime; it then builds an optimized SnapPDF binary, registers the application, and makes it the default PDF handler.

1. Clone the repository and enter it:

   ```bash
   git clone https://github.com/Rmehta-sudo/Snapdf.git
   cd Snapdf
   ```

2. Run the installer:

   ```bash
   ./scripts/install.sh
   ```

   Your system may ask for your password once to install required packages. The first build and first OCR model download can take a few minutes; later launches are normal desktop-app launches.

5. Open **SnapPDF** from the Applications menu, pin it to your dock if desired, or run:

   ```bash
   snappdf
   ```

To open a document directly:

```bash
snappdf /absolute/path/to/document.pdf
```

### Updating or removing SnapPDF

To update a cloned checkout:

```bash
git pull
./scripts/install.sh
```

To remove the desktop integration and installed executable:

```bash
./scripts/uninstall.sh
```

Your PDFs are never moved or modified by installation. SnapPDF’s local workspace state is stored at `~/.config/snappdf/state.json`; the OCR runtime and models are stored under `~/.local/share/snappdf/ocr`.

## Getting started

### Build a PDF workspace

1. Click **+ Project**, or press `Ctrl+K` then `Ctrl+O`.
2. Choose the root folder for a course, project, or document collection.
3. Expand folders in the sidebar to browse PDFs at any depth.
4. Open a document. It appears in a tab and is added to Recents.

Use the three far-left rail icons to switch between Files, document contents, and Search. The Settings icon is at the bottom of that rail.

Click a segment in the path bar to browse that directory within SnapPDF. The built-in browser presents the directory tree and its PDFs, and opening a result creates a new tab.

### OCR

Draw a rectangle with **Snip OCR** to recognize text locally. SnapPDF first uses the PDF’s embedded text layer when it is available, which is immediate and more accurate than re-reading rendered text. Otherwise it runs local OCR on a high-resolution crop. The result appears in a scrollable dialog; review it and click **Copy text** only when you want it on the clipboard.

**Math OCR (experimental)** is intended for equations and formula-heavy regions. Its quality depends on the source scan, font, and equation layout; treat its result as text to review rather than a replacement for the source PDF.

### Search

Open Search with `Ctrl+F`. Results update as you type and are organized into collapsible sections:

1. **Current file** searches the PDF you are reading.
2. **Open tabs** searches every currently open PDF.
3. **Current project** searches every PDF inside the active project’s nested folders.

Use the search panel’s Previous and Next buttons to move through results across all three sections. Results are loaded in the background so reading remains responsive.

### Annotate and share

The compact tools row provides highlighting, drawing, erasing, comments, text boxes, shapes, stamps, attachments, undo/redo, and color selection. Click a selected tool again to return to normal reading. Right-click a highlight or comment to edit or remove it.

Annotations are stored locally by default so the original file stays untouched. When you need to share the marked-up PDF, use either:

- **Export annotated copy** to write a separate copy; or
- **Save annotations into this PDF** to write compatible overlays into the original after SnapPDF creates a timestamped backup.

Use the path-row buttons to print, open the PDF in another installed app, export, or save.

## Reading modes

The controls beside the annotation tools provide three focused views:

- **Hide toolbar** leaves the sidebar and document visible.
- **Reader mode** hides the sidebar and toolbar for distraction-free continuous reading. Page gaps are reduced. Temporary zoom changes made in this mode are discarded when you exit.
- **Presentation mode** enters fullscreen and displays one page at a time. Each page fits the available screen height independently, which keeps different page sizes readable. `Space` / `Backspace`, `Right` / `Left`, and `J` / `K` move between pages. Press `Esc` or use the small top-left exit button to return. Temporary presentation zoom is discarded on exit.

## Shortcuts

| Shortcut | Action |
| --- | --- |
| `Ctrl+O` | Open a PDF |
| `Ctrl+K`, then `Ctrl+O` | Add a project folder |
| `Ctrl+F` | Open Search |
| `Ctrl+P` | Print |
| `Ctrl+W` | Close the current tab |
| `Ctrl+Shift+T` | Restore the most recently closed tab |
| `Ctrl++` / `Ctrl+-` | Scale the document and application chrome |
| `Up` / `Down` | Scroll the document |
| `Left` / `Right` | Previous / next page |
| `Ctrl+C` | Copy selected PDF text |
| `Ctrl+Z` / `Ctrl+Shift+Z` | Undo / redo an annotation |
| `Esc` | Exit Reader or Presentation mode |

## Project layout

```text
Snapdf/
├── src/          # Application, PDF rendering, storage, gestures, and OCR integration
├── resources/    # Desktop launcher, application icon, and local OCR worker
├── scripts/      # Install, OCR setup, and uninstall scripts
├── Cargo.toml    # Rust package definition
└── README.md     # User guide
```

## Build from source manually

The installer is recommended. If Rust is already installed and you only want a development run:

```bash
cargo run --release
```

Or pass a PDF path:

```bash
cargo run --release -- /absolute/path/to/document.pdf
```
