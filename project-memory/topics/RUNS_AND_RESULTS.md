# Runs and results

Record exact run/release/evaluation identities, configurations, lineage, metrics, conclusions, and
completion evidence. Do not compare configurations as controlled unless their differences are known.
# Runs and results

## 2026-10-10 scroll stress validation

- Input: `/home/rachitmehta/workspace/research-work/motion-project/paper-finds/datavista.pdf`
  (46 pages).
- Build: optimized local release after BUG-001 changes.
- Procedure: 220 rapid mouse-wheel steps down, followed by 220 rapid steps up.
- Result: process remained alive and responsive; observed RSS was approximately 195,528 KiB.
- Supporting checks: `cargo test --offline` passed both model tests; visual QA showed continuous page
  edges, small consistent inter-page spacing, vector sidebar icons, and no missing-glyph squares.

## 2026-10-10 selectable-text and Ubuntu integration release

- `cargo test --offline`: 4/4 tests passed, including stable recent ordering and display-alias state.
- `cargo build --release --offline`: completed successfully.
- Installed via `scripts/install.sh`; `application/pdf` resolves to
  `io.crisppdf.CrispPDF.desktop`.
- Launched against `Physics_of_the_Early_Universe_Part_III.pdf`; visible-page text extraction ran,
  arrow navigation was exercised, and the process remained alive.
- `xprop` reported `WM_CLASS(STRING) = "", "io.crisppdf.CrispPDF"`, matching the launcher.

## 2026-10-10 interaction polish release

- Added PDFium URI/internal-destination hit regions, hover three-dot sidebar menus, 1.65x wheel
  scrolling, held-arrow acceleration, and immediate left/right page jumps.
- `cargo test --offline`: 6/6 tests passed, including selected-text copy ordering and rejection of
  unsafe external link schemes.
- `cargo build --release --offline`: completed successfully and installed with `scripts/install.sh`.
- Held Down/Up and repeated Left/Right navigation were exercised against
  `Physics_of_the_Early_Universe_Part_III.pdf`; the process remained alive.

## 2026-10-10 local OCR benchmark

- Hardware: Ryzen 7 8845HS CPU, no usable NVIDIA CUDA runtime.
- Engine: persistent PP-OCRv5 mobile text detection/recognition on CPU, oneDNN disabled.
- Synthetic 900x180 scientific-text crop result: `Energy E = mc² and ∫ f(x) dx`.
- Warm inference reported by the worker: 254 ms. Full cold process startup plus model load and one
  inference was 2.23 seconds and used about 531 MiB peak RSS. This supports a sub-two-second warm
  crop target but is not a universal latency guarantee.

## 2026-10-10 outline, inferred-link, and restrained-UI release

- Inspected `Physics_of_the_Early_Universe_Part_III.pdf`: it exposes a deep bookmark outline but its
  first contents pages do not expose PDF link annotations.
- Added bookmark-backed Contents navigation, inferred clickable TOC/title regions, and printed URL
  detection while preserving native PDF links as authoritative.
- Math OCR no longer uses the embedded-text shortcut; formula crops route to the formula engine.
- `cargo test --offline`: 8/8 tests passed, including inferred outline and printed-URL links.
- `cargo build --release --offline`: completed successfully; `scripts/install.sh` reinstalled the
  default PDF handler. The installed app remained alive after launching the POEU Part III PDF.
- PP-FormulaNet-S acquisition stalled; formula OCR accuracy and latency remain unverified.

## 2026-10-10 link and annotation interaction correction

- Direct PDFium inspection of the exact POEU Part III PDF found 31 native links on page 4, 35 on
  page 5, and 19 on page 6; sampled named destinations resolved to valid zero-based page indices.
- Root cause: the UI only registered links while Select was active. Link registration is now
  independent of tool mode.
- Added layered annotations, highlight/comment/drawing context actions, direct click/drag erasing,
  and an installed-PDF-application chooser using safe `gio launch` dispatch.
- `cargo clippy --offline --all-targets -- -D warnings` passed.
- `cargo test --offline`: 10/10 tests passed, including comment placement above a multi-line
  highlight and external-app desktop-entry filtering.

## 2026-10-10 completed viewer and Obsidian-style UI pass

- Added startup-restored document tabs and reading positions, nested PDF-only project trees,
  document search with visible highlights, printer/page-range/copy controls, and whole-interface
  zoom with a 100% reset.
- Added highlight/comment add-edit-delete context actions, toggle-off tools, text boxes, shapes,
  signatures, stamps, attachment markers, and annotated-copy export. Export writes PDF-native
  highlights, comments, text boxes, shapes, and stamps; ink/signatures and attachment references
  remain local overlays.
- Reworked the chrome after the supplied Obsidian reference: 48 px icon rail, 340 px nested file
  sidebar, neutral dark palette, persistent document tabs, selectable/copyable path row, compact
  tool strip, and centered continuous document canvas.
- `cargo test --offline`: 10/10 passed. `cargo clippy --offline --all-targets -- -D warnings` passed.
  `cargo build --release --offline` passed. The installed binary launched with POEU Part I and a
  full-desktop screenshot confirmed the intended layout and rendered document.

## 2026-10-10 annotation interaction and zoomed-chrome correction

- Fixed the blue transient selection obscuring a newly saved highlight and prevented the PDF page
  interaction layer from intercepting highlight/comment context clicks.
- Added a test covering independently targetable overlapping comment and highlight regions.
- Replaced the crowded fixed toolbar with separated, horizontally scrollable path/action and tool
  rows; removed the visible Select control; made breadcrumb clicks copy the full path; centered the
  sidebar filter text and reduced PDF/recents icon sizes.
- `cargo test --offline`: 11/11 passed. `cargo clippy --offline --all-targets -- -D warnings` passed.
  `cargo build --release --offline` passed. Visual QA at the persisted 140% UI scale confirmed the
  breadcrumb path, neutral tool state, separated chrome, centered filter, and rendered PDF.
