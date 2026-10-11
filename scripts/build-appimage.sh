#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_ID="io.snappdf.SnapPDF"
APPDIR="${PROJECT_ROOT}/.tmp/appimage/SnapPDF.AppDir"
TOOLS_DIR="${PROJECT_ROOT}/.tmp/appimage/tools"
DIST_DIR="${PROJECT_ROOT}/dist"
APPIMAGETOOL_VERSION="1.9.1"

case "$(uname -m)" in
  x86_64)
    ARCH="x86_64"
    ;;
  *)
    echo "AppImage packaging currently supports x86_64 hosts only." >&2
    exit 1
    ;;
esac

VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "${PROJECT_ROOT}/Cargo.toml" | head -n 1)"
if [[ -z "${VERSION}" ]]; then
  echo "Could not read the SnapPDF version from Cargo.toml." >&2
  exit 1
fi

APPIMAGETOOL="${TOOLS_DIR}/appimagetool-${APPIMAGETOOL_VERSION}-${ARCH}.AppImage"
OUTPUT="${DIST_DIR}/SnapPDF-${VERSION}-${ARCH}.AppImage"

echo "Building SnapPDF ${VERSION}..."
(cd "${PROJECT_ROOT}" && cargo build --release --locked)

rm -rf "${APPDIR}"
install -d \
  "${APPDIR}/usr/bin" \
  "${APPDIR}/usr/lib/snappdf" \
  "${APPDIR}/usr/share/applications" \
  "${APPDIR}/usr/share/metainfo" \
  "${APPDIR}/usr/share/icons/hicolor/scalable/apps" \
  "${APPDIR}/usr/share/snappdf" \
  "${TOOLS_DIR}" \
  "${DIST_DIR}"

install -m 0755 "${PROJECT_ROOT}/target/release/snappdf" "${APPDIR}/usr/bin/snappdf"
install -m 0755 "${PROJECT_ROOT}/packaging/appimage/AppRun" "${APPDIR}/AppRun"
install -m 0755 "${PROJECT_ROOT}/scripts/install-ocr.sh" \
  "${APPDIR}/usr/lib/snappdf/install-ocr.sh"
install -m 0644 "${PROJECT_ROOT}/resources/ocr_worker.py" \
  "${APPDIR}/usr/share/snappdf/ocr_worker.py"
install -m 0644 "${PROJECT_ROOT}/resources/${APP_ID}.desktop" \
  "${APPDIR}/usr/share/snappdf/${APP_ID}.desktop"
install -m 0644 "${PROJECT_ROOT}/resources/${APP_ID}.svg" \
  "${APPDIR}/usr/share/icons/hicolor/scalable/apps/${APP_ID}.svg"
install -m 0644 "${PROJECT_ROOT}/resources/${APP_ID}.appdata.xml" \
  "${APPDIR}/usr/share/metainfo/${APP_ID}.appdata.xml"

sed 's|@EXEC@|snappdf|g' "${PROJECT_ROOT}/resources/${APP_ID}.desktop" \
  > "${APPDIR}/${APP_ID}.desktop"
install -m 0644 "${APPDIR}/${APP_ID}.desktop" \
  "${APPDIR}/usr/share/applications/${APP_ID}.desktop"
cp "${PROJECT_ROOT}/resources/${APP_ID}.svg" "${APPDIR}/${APP_ID}.svg"
ln -s "${APP_ID}.svg" "${APPDIR}/.DirIcon"

if [[ ! -x "${APPIMAGETOOL}" ]]; then
  echo "Downloading appimagetool ${APPIMAGETOOL_VERSION}..."
  curl --fail --location --retry 3 \
    "https://github.com/AppImage/appimagetool/releases/download/${APPIMAGETOOL_VERSION}/appimagetool-${ARCH}.AppImage" \
    --output "${APPIMAGETOOL}"
  chmod 0755 "${APPIMAGETOOL}"
fi

rm -f "${OUTPUT}"
echo "Creating ${OUTPUT##*/}..."
APPIMAGE_EXTRACT_AND_RUN=1 ARCH="${ARCH}" VERSION="${VERSION}" \
  "${APPIMAGETOOL}" "${APPDIR}" "${OUTPUT}"
chmod 0755 "${OUTPUT}"

echo
echo "Created: ${OUTPUT}"
sha256sum "${OUTPUT}"
