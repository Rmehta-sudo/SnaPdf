#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${HOME}/.local/bin"
APP_DIR="${HOME}/.local/share/applications"
ICON_DIR="${HOME}/.local/share/icons/hicolor/scalable/apps"

if [[ ! -x "${PROJECT_ROOT}/target/release/snappdf" ]]; then
  echo "Release binary not found; building it now."
  (cd "${PROJECT_ROOT}" && cargo build --release)
fi

install -d "${BIN_DIR}" "${APP_DIR}" "${ICON_DIR}"
rm -f "${BIN_DIR}/crisppdf"
rm -f "${APP_DIR}/io.crisppdf.CrispPDF.desktop"
rm -f "${ICON_DIR}/io.crisppdf.CrispPDF.svg"
install -m 0755 "${PROJECT_ROOT}/target/release/snappdf" "${BIN_DIR}/snappdf"
install -m 0644 "${PROJECT_ROOT}/resources/io.snappdf.SnapPDF.svg" \
  "${ICON_DIR}/io.snappdf.SnapPDF.svg"
sed "s|@EXEC@|${BIN_DIR}/snappdf|g" \
  "${PROJECT_ROOT}/resources/io.snappdf.SnapPDF.desktop" \
  > "${APP_DIR}/io.snappdf.SnapPDF.desktop"
chmod 0644 "${APP_DIR}/io.snappdf.SnapPDF.desktop"

command -v update-desktop-database >/dev/null && update-desktop-database "${APP_DIR}" || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -f -t "${HOME}/.local/share/icons/hicolor" >/dev/null 2>&1 || true
xdg-mime default io.snappdf.SnapPDF.desktop application/pdf

if [[ ! -d "${HOME}/.local/share/snappdf/ocr" && -d "${HOME}/.local/share/crisppdf/ocr" ]]; then
  install -d "${HOME}/.local/share/snappdf"
  mv "${HOME}/.local/share/crisppdf/ocr" "${HOME}/.local/share/snappdf/ocr"
fi

if [[ ! -x "${HOME}/.local/share/snappdf/ocr/venv/bin/python" ]] || \
  ! "${HOME}/.local/share/snappdf/ocr/venv/bin/python" -c 'import paddle, paddleocr, premailer' >/dev/null 2>&1; then
  "${PROJECT_ROOT}/scripts/install-ocr.sh"
else
  install -m 0644 "${PROJECT_ROOT}/resources/ocr_worker.py" \
    "${HOME}/.local/share/snappdf/ocr/ocr_worker.py"
fi

echo "SnapPDF installed and set as the default PDF viewer."
echo "Launch it from the application menu or run: snappdf"
