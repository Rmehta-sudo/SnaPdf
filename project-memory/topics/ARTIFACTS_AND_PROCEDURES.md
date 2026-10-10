# Artifacts and procedures

Record artifact inventories and locations plus finalized build, export, reporting, monitoring, or
operating procedures that cannot be inferred safely from code alone.
# Artifacts and procedures

## Installed application

- Release binary: `~/.local/bin/crisppdf`.
- Desktop entry: `~/.local/share/applications/io.crisppdf.CrispPDF.desktop`.
- Icon: `~/.local/share/icons/hicolor/scalable/apps/io.crisppdf.CrispPDF.svg`.
- Install/update command from the project root: `./scripts/install.sh`.
- Runtime project/recent state: `~/.config/crisppdf/state.json`.
- Local OCR environment and cached models: `~/.local/share/crisppdf/ocr/`.
- OCR installer/update command: `./scripts/install-ocr.sh` (also bootstrapped by `install.sh`).
- Dock pinning: launch CrispPDF, then right-click its dock icon and choose **Add to Favorites**. The
  running window is matched to the launcher by `io.crisppdf.CrispPDF`.
