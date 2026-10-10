# CrispPDF: read-first context

Last updated: 2026-10-10

This is the compact entry point for the project. Keep current truth here; route detail through
`project-memory/INDEX.md` and retrieve exact chat evidence only when necessary.

## Objective and success criteria

- Build a fast, crisp Ubuntu PDF viewer in Rust with a Codex-like project sidebar, PDF-only folder
  discovery, persistent global/per-project recents, and default-app integration.
- The current release succeeds when it builds, opens local PDFs smoothly, renders lazily off the UI
  thread, supports selectable/copyable text, links, outlines, local annotations/OCR and keyboard
  navigation, keeps recents stable during a session, supports display-only aliases, and installs
  without root access.

## Current architecture or operating model

- Native `egui`/`eframe` UI with a dedicated PDFium worker thread. Pages are rendered on demand at
  the current DPI/zoom and retained in a bounded texture cache.
- Obsidian-inspired chrome uses an icon rail, nested PDF-only project tree, persistent document
  tabs, selectable path bar, compact document controls, and neutral dark surfaces.
- App state is JSON in `~/.config/crisppdf/state.json`; project discovery recursively includes only
  `.pdf` files. Project names and PDF aliases are presentation-only metadata in this state.
- The Recents UI snapshots the latest 15 PDFs once at startup; opens are persisted for the next
  launch without reshuffling the current session.
- PDFium is pinned and embedded at build time through `pdfium-bundled`.

## Repositories and source-of-truth boundaries

- `/home/rachitmehta/Desktop/CrispPDF` is the authoritative source and owns its local Git repo.
- Source is under `src/`; Ubuntu integration resources are under `resources/` and `scripts/`.

## Verified current state and major results

- Release build and model tests pass as of 2026-10-10.
- The viewer survived an automated 220-step rapid scroll down and 220-step rapid scroll up through a
  46-page PDF at high zoom, remaining alive at about 191 MiB RSS.
- Installed to `~/.local/bin/crisppdf` and registered as the default `application/pdf` handler.
- The installed desktop ID, icon name, Wayland app ID, and X11 WM class all use
  `io.crisppdf.CrispPDF`, enabling reliable dock grouping and pinning.
- Visible pages expose PDFium glyph bounds for drag selection and `Ctrl+C`; arrow up/down scroll and
  arrow left/right jump to adjacent pages.
- Visible pages also expose PDFium link rectangles for safe external URLs and internal page jumps.
  Sidebar actions live in hover-only three-dot popovers; held up/down arrows accelerate scrolling.
- The sidebar has Library and Contents views. Contents is populated from the PDF bookmark tree;
  matching visible TOC text is made clickable when a PDF omits link annotations.
- Persistent highlights, ink and comments are app-owned overlays. Snip OCR uses embedded text first
  and local PP-OCRv5 for image text; Math OCR always routes image crops to the formula recognizer.
- PDF links remain interactive in every tool mode. Annotation layers are ordered highlight → ink →
  comment; right-click menus add/edit/delete highlight comments or delete annotations, and the
  eraser removes annotations directly by click or drag. Open with lists other installed PDF handlers.
- Multiple tabs, reading-position restore, nested directories, document search, CUPS print controls,
  whole-interface zoom, richer overlay tools, and annotated-copy export are implemented.
- The path strip has no visible scrollbar and each directory breadcrumb opens an in-app PDF-only
  folder browser that recursively includes PDFs at every nested depth and reports recursive counts.
  Viewer controls are icon-led and grouped into zoom, page, annotation, and history clusters;
  document zoom reaches 500% while chrome defaults to a user-editable 0.6 ratio.
- Text selection exposes Copy/Highlight actions. Comments render as unobtrusive markers with
  temporary hover cards and click-pinned cards; the Eraser uses a dedicated cursor. Recents is a
  persistent collapsible section and remains startup-frozen.
- Search is a VS Code-style sidebar view. Contents nodes support individual disclosure controls plus
  Collapse all/Expand all. Physical trackpad pinch changes only the PDF canvas, using native zoom
  events where available and Ubuntu X11 precision-touchpad gesture events otherwise.
- An explicit confirmed Save-to-PDF action can persist overlays into the source after creating a
  timestamped backup; normal annotation editing remains non-destructive by default.
- New highlights no longer retain the temporary blue selection overlay; highlight/comment context
  targets remain independently right-clickable. The path/action and tool rows scroll horizontally at
  high UI zoom, path breadcrumbs copy the full path directly, and browse mode has no visible Select
  control.

## Stable identities and paths

- Project root: `/home/rachitmehta/Desktop/CrispPDF`.
- Installed binary target: `~/.local/bin/crisppdf`.
- Desktop ID: `io.crisppdf.CrispPDF`; MIME type: `application/pdf`.

## Critical pitfalls and corrections

- The workstation network was extremely slow during bootstrap. Cargo commands should use a larger
  HTTP timeout/low-speed tolerance when crates.io is throttled; do not mistake slow transfer for a
  compiler failure.
- Page visibility must use `Ui::clip_rect()` coordinates, not the `show_viewport` callback rectangle.
  The earlier mismatch caused blank gaps. Render requests are now focused/cancelled around the
  visible page, capped at 2600 px, and held under a 256 MiB CPU texture budget.

## Open questions and next decisions

- Validate printing against a physical printer and exported annotations across third-party viewers.
- Formula-model acquisition stalled on the workstation network and remains unverified; the code path
  is implemented, but mathematical OCR quality and latency must be measured after the model arrives.
- Validate opt-in in-place annotation saving and exported annotations across additional third-party
  viewers and complex PDFs.

## Retrieval and freshness

Read `project-memory/INDEX.md` for detailed topics. The canonical facts and cold evidence must be
updated according to `project-memory/MAINTENANCE.md`; live external state must be rechecked.
