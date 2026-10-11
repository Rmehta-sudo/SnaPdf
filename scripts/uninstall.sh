#!/usr/bin/env bash
set -euo pipefail

rm -f "${HOME}/.local/bin/snapdf"
rm -f "${HOME}/.local/share/applications/io.snapdf.SnaPdf.desktop"
rm -f "${HOME}/.local/share/icons/hicolor/scalable/apps/io.snapdf.SnaPdf.svg"
rm -f "${HOME}/.local/bin/snappdf"
rm -f "${HOME}/.local/share/applications/io.snappdf.SnapPDF.desktop"
rm -f "${HOME}/.local/share/icons/hicolor/scalable/apps/io.snappdf.SnapPDF.svg"
rm -f "${HOME}/.local/bin/crisppdf"
rm -f "${HOME}/.local/share/applications/io.crisppdf.CrispPDF.desktop"
rm -f "${HOME}/.local/share/icons/hicolor/scalable/apps/io.crisppdf.CrispPDF.svg"
command -v update-desktop-database >/dev/null && update-desktop-database "${HOME}/.local/share/applications" || true

echo "SnaPdf was uninstalled. Its project/recent-document state remains in ~/.config/snapdf."
