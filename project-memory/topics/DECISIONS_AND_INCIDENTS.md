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
