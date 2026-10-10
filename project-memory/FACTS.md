# Durable fact registry

Use stable IDs, concise statements, status, one canonical destination, and inspectable provenance.

| ID | Status | Fact | Canonical detail | Provenance |
|---|---|---|---|---|
| REQ-001 | current | CrispPDF is a Rust Ubuntu PDF viewer organized around project folders and persistent recents. | `topics/GOALS_AND_DOMAIN.md` | User request, 2026-10-10 |
| REQ-002 | superseded | The initial release excluded annotations and OCR; both are now implemented as app overlays and local snip tools. | `topics/GOALS_AND_DOMAIN.md` | User request superseded, 2026-10-10 |
| DEC-001 | current | The native stack is egui/eframe plus a bundled PDFium renderer on a worker thread. | `topics/DECISIONS_AND_INCIDENTS.md` | Bootstrap implementation, 2026-10-10 |
| SYS-001 | current | The authoritative project root is `/home/rachitmehta/Desktop/CrispPDF`. | `topics/SYSTEMS_AND_PATHS.md` | User request and bootstrap, 2026-10-10 |
| SYS-002 | current | Runtime state is local JSON at `~/.config/crisppdf/state.json`. | `topics/SYSTEMS_AND_PATHS.md` | Initial architecture, 2026-10-10 |
| BUG-001 | current | Blank gaps and scroll instability were fixed by using clip-rect coordinates, focused render cancellation, reusable previews, and bounded render memory. | `topics/DECISIONS_AND_INCIDENTS.md` | Screenshot diagnosis and stress test, 2026-10-10 |
| RUN-001 | current | The release survived 220 rapid scroll steps down and 220 up on a 46-page PDF at about 191 MiB RSS. | `topics/RUNS_AND_RESULTS.md` | Automated local stress test, 2026-10-10 |
| ART-001 | current | CrispPDF is installed at `~/.local/bin/crisppdf` and registered for `application/pdf`. | `topics/ARTIFACTS_AND_PROCEDURES.md` | Installer verification, 2026-10-10 |
| REQ-003 | current | CrispPDF provides persistent non-destructive highlights, ink, comments, erase, color, and undo/redo tools. | `topics/GOALS_AND_DOMAIN.md` | User request and implementation, 2026-10-10 |
| OCR-001 | current | Snip OCR uses exact PDF text first, local PP-OCRv5 mobile for images, and PP-FormulaNet-S for LaTeX math OCR; copy is explicit. | `topics/GOALS_AND_DOMAIN.md` | User request and benchmark, 2026-10-10 |
| RUN-002 | current | Warm PP-OCRv5 mobile inference read a 900x180 scientific crop correctly in 254 ms on the local CPU. | `topics/RUNS_AND_RESULTS.md` | Local benchmark, 2026-10-10 |
| REQ-004 | current | The UI uses a restrained Obsidian-like dark visual system and Library/Contents sidebar tabs with normal top-right dialog close controls. | `topics/GOALS_AND_DOMAIN.md` | User request and implementation, 2026-10-10 |
| DEC-009 | current | Missing TOC annotations are supplemented by matching bookmark titles to visible PDF glyphs; printed HTTP(S) URLs are also inferred while native PDF links remain authoritative. | `topics/DECISIONS_AND_INCIDENTS.md` | PDF inspection and implementation, 2026-10-10 |
| OCR-002 | unverified | Math OCR is routed exclusively to PP-FormulaNet-S, but the model download stalled and output quality/latency are not yet verified locally. | `topics/RUNS_AND_RESULTS.md` | Local installation attempt, 2026-10-10 |
| RUN-003 | current | The outline/link/UI release passed 8 tests, built in release mode, installed as the default PDF viewer, and remained running with the POEU Part III PDF. | `topics/RUNS_AND_RESULTS.md` | Local build/install smoke test, 2026-10-10 |
| BUG-002 | current | Course-PDF links were valid but UI hit regions were disabled outside Select mode; links are now active in every tool mode. | `topics/DECISIONS_AND_INCIDENTS.md` | Exact PDFium inspection, 2026-10-10 |
| REQ-005 | current | Annotation layering, right-click comment/delete actions, direct click/drag erasing, and an Open-with chooser are implemented. | `topics/GOALS_AND_DOMAIN.md` | User request and implementation, 2026-10-10 |
| REQ-006 | current | The UI uses an Obsidian-inspired icon rail, nested PDF-only tree, persistent tabs, selectable path bar, and whole-interface zoom. | `topics/GOALS_AND_DOMAIN.md` | User request and implementation, 2026-10-10 |
| REQ-007 | current | Search, CUPS print controls, reading-position restore, richer annotation overlays, and annotated-copy export are implemented. | `topics/GOALS_AND_DOMAIN.md` | User request and implementation, 2026-10-10 |
| RUN-004 | current | The completed non-OCR release passed 10 tests, strict Clippy, a release build, install, launch, and full-desktop visual QA on the POEU Part I PDF. | `topics/RUNS_AND_RESULTS.md` | Local validation, 2026-10-10 |
| BUG-003 | current | Highlight creation now clears its temporary blue text selection, and annotation hit regions prevent the page layer from stealing highlight/comment context clicks. | `topics/DECISIONS_AND_INCIDENTS.md` | User report and interaction fix, 2026-10-10 |
| REQ-008 | current | The toolbar uses scrollable, separated path/action and tool rows; path breadcrumbs copy the full path directly; neutral browse mode has no visible Select button. | `topics/GOALS_AND_DOMAIN.md` | User request and implementation, 2026-10-10 |
| RUN-005 | current | The annotation/chrome correction passed 11 tests, strict Clippy, and an optimized release build with visual QA at 140% UI scale. | `topics/RUNS_AND_RESULTS.md` | Local validation, 2026-10-10 |
| BUG-004 | current | Physical trackpad pinch now uses native zoom events on Wayland and precision point-wheel gesture events on X11; line-based mouse-wheel input is excluded. | `topics/DECISIONS_AND_INCIDENTS.md` | User report and X11 backend inspection, 2026-10-11 |
| RUN-006 | current | The trackpad-pinch correction passed 14 tests, strict Clippy, an optimized release build, install, and launch on the active POEU PDF. | `topics/RUNS_AND_RESULTS.md` | Local validation, 2026-10-11 |

Statuses: `current`, `historical`, `superseded`, `unverified`, `transient`.
Use prefixes appropriate to the project, for example `REQ`, `RUN`, `BUG`, `INF`, `ART`, `MEM`.
