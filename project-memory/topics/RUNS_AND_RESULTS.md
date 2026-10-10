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
