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

- CrispPDF annotations are persistent app-owned overlays; the source PDF is never modified.
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

## Remaining later phases

- In-document search.
- Printing and print ranges.
- Embedding overlays into an exported annotated PDF rather than retaining them only in CrispPDF.
- Additional Firefox/Edge parity such as print UI, signature placement, shapes, and attachment tools.

Provenance: user request, 2026-10-10.
