# Durable fact registry

Use stable IDs, concise statements, status, one canonical destination, and inspectable provenance.

| ID | Status | Fact | Canonical detail | Provenance |
|---|---|---|---|---|
| REQ-001 | current | CrispPDF is a Rust Ubuntu PDF viewer organized around project folders and persistent recents. | `topics/GOALS_AND_DOMAIN.md` | User request, 2026-10-10 |
| REQ-002 | current | The initial release excludes annotations and OCR; they are later phases. | `topics/GOALS_AND_DOMAIN.md` | User request, 2026-10-10 |
| DEC-001 | current | The native stack is egui/eframe plus a bundled PDFium renderer on a worker thread. | `topics/DECISIONS_AND_INCIDENTS.md` | Bootstrap implementation, 2026-10-10 |
| SYS-001 | current | The authoritative project root is `/home/rachitmehta/Desktop/CrispPDF`. | `topics/SYSTEMS_AND_PATHS.md` | User request and bootstrap, 2026-10-10 |
| SYS-002 | current | Runtime state is local JSON at `~/.config/crisppdf/state.json`. | `topics/SYSTEMS_AND_PATHS.md` | Initial architecture, 2026-10-10 |
| BUG-001 | current | Blank gaps and scroll instability were fixed by using clip-rect coordinates, focused render cancellation, reusable previews, and bounded render memory. | `topics/DECISIONS_AND_INCIDENTS.md` | Screenshot diagnosis and stress test, 2026-10-10 |
| RUN-001 | current | The release survived 220 rapid scroll steps down and 220 up on a 46-page PDF at about 191 MiB RSS. | `topics/RUNS_AND_RESULTS.md` | Automated local stress test, 2026-10-10 |
| ART-001 | current | CrispPDF is installed at `~/.local/bin/crisppdf` and registered for `application/pdf`. | `topics/ARTIFACTS_AND_PROCEDURES.md` | Installer verification, 2026-10-10 |

Statuses: `current`, `historical`, `superseded`, `unverified`, `transient`.
Use prefixes appropriate to the project, for example `REQ`, `RUN`, `BUG`, `INF`, `ART`, `MEM`.
