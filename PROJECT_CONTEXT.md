# CrispPDF: read-first context

Last updated: 2026-10-10

This is the compact entry point for the project. Keep current truth here; route detail through
`project-memory/INDEX.md` and retrieve exact chat evidence only when necessary.

## Objective and success criteria

- Build a fast, crisp Ubuntu PDF viewer in Rust with a Codex-like project sidebar, PDF-only folder
  discovery, persistent global/per-project recents, and default-app integration.
- The initial release succeeds when it builds, opens local PDFs smoothly, renders lazily off the UI
  thread, persists projects/recents, and installs without root access.

## Current architecture or operating model

- Native `egui`/`eframe` UI with a dedicated PDFium worker thread. Pages are rendered on demand at
  the current DPI/zoom and retained in a bounded texture cache.
- App state is JSON in `~/.config/crisppdf/state.json`; project discovery recursively includes only
  `.pdf` files.
- PDFium is pinned and embedded at build time through `pdfium-bundled`.

## Repositories and source-of-truth boundaries

- `/home/rachitmehta/Desktop/CrispPDF` is the authoritative source and owns its local Git repo.
- Source is under `src/`; Ubuntu integration resources are under `resources/` and `scripts/`.

## Verified current state and major results

- Release build and model tests pass as of 2026-10-10.
- The viewer survived an automated 220-step rapid scroll down and 220-step rapid scroll up through a
  46-page PDF at high zoom, remaining alive at about 191 MiB RSS.
- Installed to `~/.local/bin/crisppdf` and registered as the default `application/pdf` handler.

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

- Validate rendering and UI against representative large/text-heavy PDFs.
- Later phases: selectable text/search, printing, annotations/comments, then region capture + OCR.

## Retrieval and freshness

Read `project-memory/INDEX.md` for detailed topics. The canonical facts and cold evidence must be
updated according to `project-memory/MAINTENANCE.md`; live external state must be rechecked.
