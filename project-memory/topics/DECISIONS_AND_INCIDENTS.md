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

## BUG-002: valid TOC links gated by the active tool

The POEU Part III contents pages do contain native `/Link` annotations with valid same-document
`/GoTo` destinations. Direct PDFium inspection resolved 31 links on page 4 and 35 on page 5. The
viewer nevertheless registered link hit regions only in Select mode, so switching to any annotation
tool made them appear inert. Register link interactions in every tool mode and retain the outline/text
fallback only for genuinely unannotated documents.

## DEC-011: annotation interaction and z-order

Paint highlights first, ink second, and comments last. Register annotation context targets that offer
Add comment/Delete highlight for highlights and type-specific Delete for comments and drawings. The
Eraser uses annotation hit-testing for direct click-and-drag removal with undo history; it never uses
the OCR snip rectangle. Comments created from highlights anchor just beyond the highlight's top-right
edge.

## DEC-012: external application chooser

Discover visible `.desktop` entries that advertise `application/pdf`, exclude CrispPDF itself and
duplicate IDs, present them in a conventional top-right-close dialog, and launch the chosen entry
through `gio launch` rather than parsing or executing its `Exec=` line directly.

## BUG-003: transient selection obscured highlights and page hit layer stole context clicks

After a highlight was committed, its source text selection remained active and painted blue above
the saved annotation until the next click. Clear that transient selection immediately after saving.
The full-page tool interaction was also registered over annotation responses, preventing reliable
secondary-click menus. Detect annotation hits first and suppress the broad page interaction there
(except for active erasing), so highlight and comment menus receive right-clicks independently.

## DEC-013: zoom-resilient document chrome

Keep document tabs, path/actions, and tools as distinct rows with separators. The path and tool rows
are horizontally scrollable by wheel, trackpad, or direct drag, so whole-interface zoom cannot
overlap or hide controls. Hide the native scrollbar because its high-contrast trough overwhelms the
compact header. Render the path as Obsidian-style component breadcrumbs; clicking any component
copies the full path. Browse/text-selection remains the internal default but has no visible Select
button; clicking an active annotation tool again returns to that neutral mode.

## DEC-014: compact document controls and transient annotation detail

Hide horizontal scrollbar chrome entirely while preserving trackpad, wheel, and drag scrolling.
Group zoom, editable page position, tools, and undo/redo with separators and vector-drawn icons.
Breadcrumb directories switch the center region to a cached PDF-only folder browser instead of
launching an external file manager. Its unbounded recursive scan runs on a background thread so even
deep trees remain complete without freezing the viewer. Comment markers do not paint their text over the PDF: hover shows
a temporary card, and primary click pins it until explicitly closed. Text selections expose a small
Copy/Highlight action card, and Erase hides the system pointer in favor of a drawn eraser cursor.

Recents has a persisted expand/collapse flag but retains the startup-frozen 15-item order. Document
zoom is allowed through 500%. Superseding the earlier fixed 0.7 relationship, chrome now defaults to
a persisted, View-settings-editable 0.6 ratio across tabs, path, tools, rail, and sidebar.

## DEC-015: sidebar search, hierarchical contents, and opt-in source saving

Search belongs to a dedicated VS Code-style sidebar view; the document header does not grow a search
row. Contents keeps its flat PDFium outline representation internally but derives parent/child
visibility by depth, preserving individual collapsed-node indices and offering Collapse all/Expand
all.

Normal edits remain app-owned overlays. The explicit Save-to-PDF action exports to a temporary PDF
beside the source, copies the original to a timestamped backup, and atomically replaces the source
only after export succeeds. Highlight comments become highlight contents, freehand/signature paths
become PDF path objects, and attachment references become PDF text annotations. Successful saving
clears the now-embedded overlays before reopening the PDF to prevent double rendering.

## BUG-004: PDF-only trackpad pinch was not recognized on the X11 desktop

The workstation runs an X11 session, where winit does not provide the same dedicated pinch event
available on Wayland. Ubuntu represents a physical precision-touchpad pinch as point-granularity
wheel input with a synthetic modifier. Detect that signature alongside native `Event::Zoom`, apply
it only while the pointer is in the PDF viewport, and exclude line-granularity mouse-wheel events.
This preserves two-finger trackpad pinch without requiring the user to hold Ctrl and without scaling
the sidebar or document chrome.
# 2026-10-10 direct link hit testing and export boundary

- PDF links are now resolved from the latest raw pointer position before the page tool interaction
  layer. This avoids response-order competition between link rectangles, text selection, and active
  annotation tools; link hover always gets the pointing-hand cursor and primary click activation.
- Annotated-copy export uses PDFium-native annotations for highlights, comments, free text, shapes,
  and stamps. Freehand ink/signatures and local attachment references remain in CrispPDF state until
  a reliable cross-viewer PDF representation is implemented.
