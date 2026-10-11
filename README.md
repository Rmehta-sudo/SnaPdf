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
- **A study assistant beside the page.** Open ChatGPT or Gemini on the right, choose a model, and ask questions with an OCR text snip, a captured PDF figure, an image, or an entire PDF attached.

**No imports, duplicate files, or cloud storage are required.** Your PDFs stay in their original locations; SnapPDF simply makes them easier to navigate, read, search, and return to.

In short, SnapPDF is not just another PDF viewer. It is a fast, filesystem-first workspace for managing and reading PDFs across multiple projects.

## Install SnapPDF — no Git or development tools

The recommended installation is the portable, checksum-verified AppImage. It installs only for your user account: no Rust, Cargo, compiler, `apt`, administrator password, or repository clone is needed.

### Easiest safe installation

Download the small installer, inspect it if you wish, then run it:

```bash
curl --proto '=https' --tlsv1.2 -fLO \
  https://github.com/Rmehta-sudo/Snapdf/releases/latest/download/install-snappdf.sh
less install-snappdf.sh
sh install-snappdf.sh
```

The installer discovers the latest release, downloads the AppImage and its published `SHA256SUMS`, refuses to continue unless the checksum matches, and places SnapPDF under your home directory. It also creates the application-menu entry and icon. It never uses `sudo` and does not modify system directories.

Afterward, open **SnapPDF** from the application menu and pin it to your dock, or run `snappdf` from a terminal. You can delete the downloaded `install-snappdf.sh` once installation finishes.

Portable releases currently target **x86_64 Linux desktops**. The launcher uses AppImage's extract-and-run mode, so it does not require FUSE. Other CPU architectures can still use the source build until matching portable releases are added.

### Install the AppImage manually

Download the latest `SnapPDF-…-x86_64.AppImage` from the [SnapPDF releases page](https://github.com/Rmehta-sudo/Snapdf/releases/latest), then make it executable and open it:

```bash
chmod +x SnapPDF-*.AppImage
./SnapPDF-*.AppImage
```

For a manual integrity check, download `SHA256SUMS` from the same release into the same directory and run `sha256sum --check --ignore-missing SHA256SUMS` before opening the AppImage.

That is the complete installation for the PDF workspace. On first launch SnapPDF registers its application entry and icon for your user account, so you can find it in the application menu and pin it to your dock. If you move the AppImage later, launch it once from the new location to refresh that entry.

To open a document directly, pass its path to the same file:

```bash
./SnapPDF-*.AppImage /absolute/path/to/document.pdf
```

### Optional offline OCR setup

The PDF viewer and annotation workspace work immediately. Offline OCR uses larger local machine-learning libraries and models, so they are not added to every SnapPDF download. Install them once through the AppImage when you want OCR:

```bash
./SnapPDF-*.AppImage --install-ocr
```

This downloads the OCR runtime into `~/.local/share/snappdf/ocr`; the OCR models download on their first use. It does not require Rust or system-wide packages.

### Updating or removing SnapPDF

To update, download the newer AppImage and replace the old one. To remove SnapPDF, delete its AppImage. Your PDFs are never moved or modified by installation.

SnapPDF’s workspace state is stored at `~/.config/snappdf/state.json`; connected study-chat credentials are stored separately at `~/.config/snappdf/chat-credentials.json` with owner-only permissions; its optional OCR runtime and models are stored under `~/.local/share/snappdf/ocr`. Remove those paths too only if you also want to erase saved workspace data, provider connections, and OCR data.

### Build and install from source

Developers who want a native local build can clone the repository and run the source installer:

```bash
git clone https://github.com/Rmehta-sudo/Snapdf.git
cd Snapdf
./scripts/install.sh
```

The source installer installs compilation dependencies on systems with `apt`, installs Rust when needed, builds SnapPDF, registers the desktop entry, and sets up OCR. Those build dependencies are needed only for compiling from source—not for the AppImage.

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

### Study chat

Click the chat-bubble button in the document toolbar to open the resizable panel on the right. Choose a provider and model, then use it as a focused companion while reading—ordinary chat, not an autonomous agent.

- **ChatGPT:** click **Continue with ChatGPT**. Sign-in happens in your normal browser through OpenAI's [official open-source account flow](https://developers.openai.com/siwc/token-sharing-open-source); SnapPDF never sees your password. Available models are loaded from your eligible account after sign-in.
- **Gemini:** click **Connect Gemini** and paste a key created in [Google AI Studio](https://aistudio.google.com/app/apikey). Available Gemini models are loaded after the key is validated. A direct Google-account sign-in requires SnapPDF to have a registered Google OAuth client, so the portable open-source build deliberately uses the official user-owned API-key route instead of pretending to provide OAuth.

Use the `＋` button in the composer to attach a PDF, image, or text file. **Attach current PDF** sends the document you are reading. The capture button beside it changes the cursor into a crosshair; drag around a diagram, equation, map, or paragraph and the exact rendered region is added as an image—without sending it through Math OCR.

Select ordinary PDF text and click **Ask** to attach it directly. After a normal Snip OCR operation, click **Add to study chat** to do the same with recognized text; it is not copied to the clipboard. Attachments remain local until you press **Send**. Each provider keeps a separate on-screen conversation; when you send, SnapPDF transmits that conversation's history and chosen attachments to that provider so follow-up questions retain context. SnapPDF does not upload projects or PDFs in the background.

`Enter` sends a chat message; `Shift+Enter` adds another line. Remove an attachment with its `×` before sending, or close the study panel without affecting the PDF.

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
| `Ctrl+Tab` | Move to the next open PDF tab |
| `Ctrl+Shift+Tab` | Move to the previous open PDF tab |
| `Ctrl++` / `Ctrl+-` | Scale the document and application chrome |
| `Up` / `Down` | Scroll the document |
| `Left` / `Right` | Previous / next page |
| `Ctrl+C` | Copy selected PDF text |
| `Ctrl+Z` / `Ctrl+Shift+Z` | Undo / redo an annotation |
| `Esc` | Exit Reader or Presentation mode |

## Project layout

```text
Snapdf/
├── packaging/    # AppImage launcher and packaging files
├── src/          # Application, PDF rendering, study chat, storage, gestures, and OCR integration
├── resources/    # Desktop launcher, application icon, and local OCR worker
├── scripts/      # Verified portable install, AppImage build, source install, OCR, and uninstall scripts
├── Cargo.toml    # Rust package definition
└── README.md     # User guide
```

## Build from source manually

For normal use, the AppImage is recommended. If Rust is already installed and you only want a development run:

```bash
cargo run --release
```

Or pass a PDF path:

```bash
cargo run --release -- /absolute/path/to/document.pdf
```
