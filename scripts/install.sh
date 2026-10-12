#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${HOME}/.local/bin"
APP_DIR="${HOME}/.local/share/applications"
ICON_DIR="${HOME}/.local/share/icons/hicolor/scalable/apps"

ensure_system_dependencies() {
  if ! command -v apt-get >/dev/null 2>&1; then
    return
  fi
  echo "Installing Linux build dependencies (your password may be requested)..."
  sudo apt-get update
  sudo apt-get install -y \
    build-essential pkg-config curl ca-certificates \
    libfontconfig1-dev libfreetype6-dev libxkbcommon-dev libwayland-dev \
    libx11-dev libxrandr-dev libxi-dev libgl1-mesa-dev libgtk-3-dev
}

if ! command -v cargo >/dev/null 2>&1; then
  ensure_system_dependencies
  echo "Installing the Rust toolchain..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  export PATH="${HOME}/.cargo/bin:${PATH}"
fi

if ! command -v uv >/dev/null 2>&1 && [[ ! -x "${HOME}/.local/bin/uv" ]]; then
  echo "Installing the local OCR runtime..."
  curl -LsSf https://astral.sh/uv/install.sh | sh
fi
export PATH="${HOME}/.local/bin:${HOME}/.cargo/bin:${PATH}"

# Always check the current source. Cargo's incremental build cache makes this
# quick when nothing changed; skipping the build would reinstall a stale binary.
echo "Building the current SnaPdf source..."
(cd "${PROJECT_ROOT}" && cargo build --release --locked)

install -d "${BIN_DIR}" "${APP_DIR}" "${ICON_DIR}"
rm -f "${BIN_DIR}/crisppdf"
rm -f "${BIN_DIR}/snappdf"
rm -f "${APP_DIR}/io.crisppdf.CrispPDF.desktop"
rm -f "${ICON_DIR}/io.crisppdf.CrispPDF.svg"
rm -f "${APP_DIR}/io.snappdf.SnapPDF.desktop"
rm -f "${ICON_DIR}/io.snappdf.SnapPDF.svg"
install -m 0755 "${PROJECT_ROOT}/target/release/snapdf" "${BIN_DIR}/snapdf"
install -m 0755 "${PROJECT_ROOT}/scripts/uninstall.sh" "${BIN_DIR}/snapdf-uninstall"
install -m 0644 "${PROJECT_ROOT}/resources/io.snapdf.SnaPdf.svg" \
  "${ICON_DIR}/io.snapdf.SnaPdf.svg"
sed "s|@EXEC@|${BIN_DIR}/snapdf|g" \
  "${PROJECT_ROOT}/resources/io.snapdf.SnaPdf.desktop" \
  > "${APP_DIR}/io.snapdf.SnaPdf.desktop"
chmod 0644 "${APP_DIR}/io.snapdf.SnaPdf.desktop"

command -v update-desktop-database >/dev/null && update-desktop-database "${APP_DIR}" || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -f -t "${HOME}/.local/share/icons/hicolor" >/dev/null 2>&1 || true

if [[ ! -e "${HOME}/.local/share/snapdf" && -d "${HOME}/.local/share/snappdf" ]]; then
  mv "${HOME}/.local/share/snappdf" "${HOME}/.local/share/snapdf"
elif [[ ! -d "${HOME}/.local/share/snapdf/ocr" && -d "${HOME}/.local/share/snappdf/ocr" ]]; then
  install -d "${HOME}/.local/share/snapdf"
  mv "${HOME}/.local/share/snappdf/ocr" "${HOME}/.local/share/snapdf/ocr"
fi

if [[ ! -d "${HOME}/.local/share/snapdf/ocr" && -d "${HOME}/.local/share/crisppdf/ocr" ]]; then
  install -d "${HOME}/.local/share/snapdf"
  mv "${HOME}/.local/share/crisppdf/ocr" "${HOME}/.local/share/snapdf/ocr"
fi

if [[ ! -x "${HOME}/.local/share/snapdf/ocr/venv/bin/python" ]] || \
  ! "${HOME}/.local/share/snapdf/ocr/venv/bin/python" -c 'import paddle, paddleocr, premailer' >/dev/null 2>&1; then
  "${PROJECT_ROOT}/scripts/install-ocr.sh"
else
  install -m 0644 "${PROJECT_ROOT}/resources/ocr_worker.py" \
    "${HOME}/.local/share/snapdf/ocr/ocr_worker.py"
fi

echo "SnaPdf installed. Your existing default PDF viewer was not changed."
echo "Launch it from the application menu or run: snapdf"
