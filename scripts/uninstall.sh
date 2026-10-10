#!/usr/bin/env bash
set -euo pipefail

rm -f "${HOME}/.local/bin/crisppdf"
rm -f "${HOME}/.local/share/applications/io.crisppdf.CrispPDF.desktop"
rm -f "${HOME}/.local/share/icons/hicolor/scalable/apps/io.crisppdf.CrispPDF.svg"
command -v update-desktop-database >/dev/null && update-desktop-database "${HOME}/.local/share/applications" || true

echo "CrispPDF was uninstalled. Its project/recent-document state remains in ~/.config/crisppdf."

