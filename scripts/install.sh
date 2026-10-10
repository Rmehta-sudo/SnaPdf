#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${HOME}/.local/bin"
APP_DIR="${HOME}/.local/share/applications"
ICON_DIR="${HOME}/.local/share/icons/hicolor/scalable/apps"

if [[ ! -x "${PROJECT_ROOT}/target/release/crisppdf" ]]; then
  echo "Release binary not found; building it now."
  (cd "${PROJECT_ROOT}" && cargo build --release)
fi

install -d "${BIN_DIR}" "${APP_DIR}" "${ICON_DIR}"
install -m 0755 "${PROJECT_ROOT}/target/release/crisppdf" "${BIN_DIR}/crisppdf"
install -m 0644 "${PROJECT_ROOT}/resources/io.crisppdf.CrispPDF.svg" \
  "${ICON_DIR}/io.crisppdf.CrispPDF.svg"
sed "s|@EXEC@|${BIN_DIR}/crisppdf|g" \
  "${PROJECT_ROOT}/resources/io.crisppdf.CrispPDF.desktop" \
  > "${APP_DIR}/io.crisppdf.CrispPDF.desktop"
chmod 0644 "${APP_DIR}/io.crisppdf.CrispPDF.desktop"

command -v update-desktop-database >/dev/null && update-desktop-database "${APP_DIR}" || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -f -t "${HOME}/.local/share/icons/hicolor" >/dev/null 2>&1 || true
xdg-mime default io.crisppdf.CrispPDF.desktop application/pdf

if [[ ! -x "${HOME}/.local/share/crisppdf/ocr/venv/bin/python" ]] || \
  ! "${HOME}/.local/share/crisppdf/ocr/venv/bin/python" -c 'import paddle, paddleocr, premailer' >/dev/null 2>&1; then
  "${PROJECT_ROOT}/scripts/install-ocr.sh"
else
  install -m 0644 "${PROJECT_ROOT}/resources/ocr_worker.py" \
    "${HOME}/.local/share/crisppdf/ocr/ocr_worker.py"
fi

echo "CrispPDF installed and set as the default PDF viewer."
echo "Launch it from the application menu or run: crisppdf"
