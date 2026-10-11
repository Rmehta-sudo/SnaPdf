#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OCR_ROOT="${HOME}/.local/share/snapdf/ocr"
UV_BIN="${HOME}/.local/bin/uv"
WORKER_SOURCE="${SNAPDF_OCR_WORKER:-${PROJECT_ROOT}/resources/ocr_worker.py}"

if [[ ! -x "${UV_BIN}" ]]; then
  UV_BIN="$(command -v uv || true)"
fi
if [[ -z "${UV_BIN}" ]]; then
  echo "Installing the OCR environment manager..."
  curl --proto '=https' --tlsv1.2 -LsSf https://astral.sh/uv/install.sh | sh
  UV_BIN="${HOME}/.local/bin/uv"
fi
if [[ ! -x "${UV_BIN}" ]]; then
  echo "Could not install the OCR environment manager." >&2
  exit 1
fi

install -d "${OCR_ROOT}"
if [[ ! -x "${OCR_ROOT}/venv/bin/python" ]]; then
  "${UV_BIN}" venv --python 3.12 "${OCR_ROOT}/venv"
fi
UV_CACHE_DIR="${HOME}/.cache/snapdf/uv" UV_HTTP_TIMEOUT=600 "${UV_BIN}" pip install \
  --quiet \
  --python "${OCR_ROOT}/venv/bin/python" \
  "paddleocr[doc-parser]==3.7.0" "paddlepaddle>=3.2,<4"
install -m 0644 "${WORKER_SOURCE}" "${OCR_ROOT}/ocr_worker.py"

echo "SnaPdf local OCR installed. Models download once on first launch."
