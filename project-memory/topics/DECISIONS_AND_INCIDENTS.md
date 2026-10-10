# Decisions and incidents

Record durable decisions, error signatures, failed approaches, causal diagnoses, fixes, validation,
and supersession history. Prefer cause/fix/provenance over full logs.

## DEC-001: native renderer/UI stack

Use `egui`/`eframe` for a compact native GPU UI and `pdfium-bundled` for PDFium rendering. Embed the
pinned PDFium shared library in the release executable. A dedicated worker thread owns PDFium and
the open document; the UI receives RGBA page buffers through channels and uploads textures on the
main thread. This avoids extra Ubuntu development/runtime packages and prevents rendering from
blocking input.

## INC-001: slow dependency network

During bootstrap, crates.io transfers repeatedly hit Cargo's default low-speed timeout, matching the
earlier observed slow Flathub connection. Retry with `CARGO_HTTP_TIMEOUT=600`,
`CARGO_HTTP_LOW_SPEED_LIMIT=1`, and `CARGO_HTTP_MULTIPLEXING=false` while the network is degraded.

## BUG-001: blank page gaps and unstable fast scrolling

The first viewer compared allocated page rectangles against the `show_viewport` callback rectangle,
which was not in the same coordinate space. This could skip visible pages. It also cached textures by
exact zoom width and allowed large pending high-resolution renders, creating avoidable memory
pressure while scrolling.

The fix uses `Ui::clip_rect()` for visibility, lays out pages before selecting the focused page,
cancels renders more than two pages from focus, caps pending requests at six and render width at
2600 px, reuses the previous page texture while a sharper version arrives, and evicts above ten
textures or 256 MiB of CPU image data. The sidebar replaced unsupported Unicode icon characters
with vector-drawn icons. Project files remain in stable alphabetical order; recency only affects the
separate Recents section.

## DEC-002: stable recents and display aliases

The sidebar takes a normalized, 15-item recents snapshot at application startup. Opening a PDF still
updates persistent timestamps, but the visible order does not change until the next launch. Project
renames update `Project.name`; PDF renames update a path-keyed alias map. Neither operation renames
or moves anything in the filesystem.

## DEC-003: selectable text without replacing the renderer

Keep the proven PDFium bitmap renderer and asynchronously extract per-character Unicode values and
loose glyph bounds for visible pages on the same worker. The UI maps PDF bottom-left coordinates to
page rectangles, paints selection highlights, and copies the selected character range with
`Ctrl+C`. This adds a real text layer without putting PDFium calls on the UI thread.

## DEC-004: Ubuntu dock identity

Use `io.crisppdf.CrispPDF` consistently as the eframe app ID, desktop entry ID, icon name, and
`StartupWMClass`. X11 validation returned that exact WM class, allowing GNOME to group the running
window with the installed launcher and icon.

## DEC-005: safe clickable links and accelerated navigation

Extract URI and same-document destinations with the visible-page PDFium metadata job. Map their PDF
rectangles into the rendered page, prioritize link clicks over text selection, open only `http`,
`https`, and `mailto` externally, and route internal destinations through exact page jumps. Wheel
motion is multiplied by 1.65; held vertical arrows ramp from 850 to 3,200 points/second while direct
left/right page jumps remain immediate.

## DEC-006: sidebar actions use hover menus

Every project and PDF row always owns a stable options-button ID, but its ASCII three-dot label is
painted only while the row is hovered. Clicking opens a rounded, high-contrast popup with clear
rename/reset/remove grouping. ASCII dots avoid the missing-symbol issue from the first release.

## DEC-007: non-destructive annotation overlays

Store highlights, ink paths, and comments in `~/.config/crisppdf/state.json` using PDF coordinates.
Render them over pages and keep the original PDF byte-for-byte unchanged. Maintain per-document
in-memory undo/redo snapshots; exporting overlays into a PDF remains separate future work.

## DEC-008: hybrid local OCR

Use exact embedded PDF glyphs as the zero-latency first path. For image-only snips, render only the
selected region at high resolution and send it to a persistent local Python worker. PP-OCRv5 mobile
handles normal text; the separate PP-FormulaNet-S mode returns LaTeX for formula crops. The worker
uses CPU-only PaddlePaddle with oneDNN disabled because PaddlePaddle 3.3.1 raised
`ConvertPirAttribute2RuntimeAttribute` for the mobile OCR graph when oneDNN was enabled.

Math OCR must not take the embedded-text shortcut used by normal Snip OCR: the selected image region
is always sent to PP-FormulaNet-S so its output is LaTeX. The formula model download stalled on the
workstation network, so this path is implemented but not yet quality- or latency-verified.

## DEC-009: outline-backed inferred links

Some course PDFs contain a complete bookmark tree but no `/Annots` entries on their printed contents
pages. Keep native PDFium link annotations authoritative, then supplement non-overlapping regions by
normalizing bookmark titles and matching them against extracted glyph text. Also infer visibly
printed HTTP(S) and `www.` addresses. The Contents sidebar renders the same bookmark tree and routes
destinations through the same safe-link/page-jump handler.

## DEC-010: restrained visual language

Use neutral dark surfaces, thin separators, compact row heights, one restrained blue accent, and
hover/selection states rather than persistent multicolor cards. Library and Contents are sidebar
views, while project folders and files retain hierarchy through indentation and typography.
