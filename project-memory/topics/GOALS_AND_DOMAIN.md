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

## Explicit later phases

- Text layer, selection/copy, and in-document search.
- Printing and print ranges.
- Highlighting, comments, annotations, and safe PDF saves.
- Region screenshot followed by OCR/extracted text in a right-side panel.

Provenance: user request, 2026-10-10.
