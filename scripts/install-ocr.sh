#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OCR_ROOT="${HOME}/.local/share/crisppdf/ocr"
UV_BIN="${HOME}/.local/bin/uv"

if [[ ! -x "${UV_BIN}" ]]; then
  UV_BIN="$(command -v uv || true)"
fi
if [[ -z "${UV_BIN}" ]]; then
  echo "uv is required to install local OCR. Install uv, then run this script again." >&2
  exit 1
fi

install -d "${OCR_ROOT}"
if [[ ! -x "${OCR_ROOT}/venv/bin/python" ]]; then
  "${UV_BIN}" venv --python 3.12 "${OCR_ROOT}/venv"
fi
UV_CACHE_DIR="${HOME}/.cache/crisppdf/uv" UV_HTTP_TIMEOUT=600 "${UV_BIN}" pip install \
  --quiet \
  --python "${OCR_ROOT}/venv/bin/python" \
  "paddleocr[doc-parser]==3.7.0" "paddlepaddle>=3.2,<4"
install -m 0644 "${PROJECT_ROOT}/resources/ocr_worker.py" "${OCR_ROOT}/ocr_worker.py"

echo "CrispPDF local OCR installed. Models download once on first launch."
