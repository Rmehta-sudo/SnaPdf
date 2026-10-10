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
