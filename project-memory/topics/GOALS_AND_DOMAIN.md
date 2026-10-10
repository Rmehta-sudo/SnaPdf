# Goals and domain

Record objectives, success criteria, terminology, requirements, hypotheses, assumptions, and open
questions. Distinguish verified facts from interpretations and proposals.

## Product goal

CrispPDF should feel like a focused Edge/Firefox-class viewer while making research and course PDFs
easy to retrieve through durable project folders. The left sidebar shows global recents, projects,
per-project recents, and recursively discovered PDFs; it never lists non-PDF files.

## Initial-release scope

- Native Ubuntu app written in Rust.
- Fast background PDF rendering, high-DPI textures, continuous scrolling, zoom, and fit width.
- Add/remove/refresh project folders, PDF-only filtering, filename search, drag-and-drop.
- Persistent global and project-scoped recents.
- Application-menu and `application/pdf` default-app integration.
- Stable app/dock identity and a dedicated icon.
- A startup-frozen list of the 15 most recent PDFs, with new opens affecting the next launch only.
- Arrow-key scrolling/page navigation, selectable/copyable PDF text, and display-only project/PDF
  renaming.
- Clickable PDF links, hover three-dot option menus, faster animated wheel scrolling, and
  acceleration while navigation arrows are held.

## Current annotation and OCR release

- CrispPDF annotations are persistent app-owned overlays by default. A separate confirmed action may
  write them into the source PDF after creating a timestamped backup.
- Selectable text supports copy; annotation tools cover highlights, freehand ink, comments, erasing,
  color choice, and undo/redo.
- Snip OCR first uses the exact embedded PDF text layer. Image-only crops use a persistent local
  PP-OCRv5 mobile CPU worker; Math OCR uses PP-FormulaNet-S and returns LaTeX. OCR results appear in
  an editable popup with an explicit Copy button and are never copied automatically.
- The sidebar uses restrained neutral surfaces and a single accent instead of colored project cards.
  Library and Contents tabs separate document organization from the current PDF bookmark hierarchy;
  recents remain startup-frozen.
- Rename, comment, and OCR result windows use conventional top-right close controls.
- Existing PDF links and inferred contents-page/printed-URL links should open websites safely or
  navigate to the destination page within the PDF.
- Links stay interactive in every annotation mode. Highlights provide right-click add-comment and
  delete actions; comments and drawings provide right-click delete actions. Comments paint above
  highlights, while the Erase tool directly removes annotations by clicking or dragging over them.
- Any PDF shown in the sidebar or viewer can be opened in another installed PDF-capable application
  through an in-app chooser.
- Neutral browse/text-selection is implicit rather than exposed as a selected tool. Path components
  copy the full path when clicked, and path/actions/tools remain usable at high interface zoom via
  separated horizontal scrolling rows.
- Directory breadcrumb components open a PDF-only browser in the main region. It recursively lists
  every PDF beneath that directory regardless of nesting depth; choosing one opens it in a tab and
  records it for the next launch's Recents ordering. The final filename component copies the path.
- Recents can be collapsed independently like project folders. Comment text is hidden until marker
  hover or click, text selection offers Copy/Highlight, and toolbar actions use compact icons.
- Document zoom supports 70% through 500%; surrounding chrome, including the navigation rail and
  sidebar, defaults to a user-editable 0.6 ratio. Pinch gestures zoom only the PDF canvas.
- Document search uses a dedicated VS Code-style sidebar. Bookmark-outline sections and subsections
  can be collapsed individually or globally.
- Folder browsing uses a denser file-manager table with recursive PDF counts and an explicit action
  to open the directory in Ubuntu Files.

## Remaining later phases

- Validate printing with a physical printer and exported annotations in third-party viewers.
- Validate opt-in in-place PDF annotation saving across additional external viewers.
- Revisit local mathematical OCR quality and latency after the formula model acquisition succeeds.

Provenance: user request, 2026-10-10.
# 2026-10-10 completed viewer/UI expansion

- Project folders render as the real nested filesystem hierarchy while filtering out non-PDFs.
- Open PDFs persist as tabs, each document restores its last visible page, and the full absolute path
  is visible, selectable, and copyable.
- `Ctrl++`, `Ctrl+-`, and `Ctrl+0` scale the complete interface rather than only page content.
- Search, CUPS printing, richer overlay annotation tools, and annotated-copy export are part of the
  viewer. Highlights and comments have contextual add/edit/delete actions; active tools toggle off.
- The visual target is the supplied Obsidian PDF-viewer reference: neutral dark surfaces, narrow icon
  rail, understated nested tree, compact tabs/path/actions, and a centered continuous PDF canvas.
