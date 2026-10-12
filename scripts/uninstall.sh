#!/usr/bin/env bash
set -euo pipefail

purge=false
assume_yes=false

usage() {
  cat <<'EOF'
Usage: snapdf-uninstall [--purge] [--yes]

Remove SnaPdf from the current user account.

  --purge  Also erase saved projects, reading positions, annotations,
           study chats, credentials, settings, and downloaded OCR data.
  --yes    Skip the confirmation required by --purge.
  -h       Show this help.

Without --purge, personal data is preserved for a future reinstall.
EOF
}

for argument in "$@"; do
  case "$argument" in
    --purge) purge=true ;;
    --yes) assume_yes=true ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown option: $argument" >&2; usage >&2; exit 2 ;;
  esac
done

data_home="${XDG_DATA_HOME:-${HOME}/.local/share}"
config_home="${XDG_CONFIG_HOME:-${HOME}/.config}"
bin_dir="${HOME}/.local/bin"

rm -f \
  "${bin_dir}/snapdf" \
  "${data_home}/snapdf/app/SnaPdf.AppImage" \
  "${data_home}/applications/io.snapdf.SnaPdf.desktop" \
  "${data_home}/icons/hicolor/scalable/apps/io.snapdf.SnaPdf.svg" \
  "${data_home}/metainfo/io.snapdf.SnaPdf.appdata.xml" \
  "${bin_dir}/snappdf" \
  "${data_home}/applications/io.snappdf.SnapPDF.desktop" \
  "${data_home}/icons/hicolor/scalable/apps/io.snappdf.SnapPDF.svg" \
  "${bin_dir}/crisppdf" \
  "${data_home}/applications/io.crisppdf.CrispPDF.desktop" \
  "${data_home}/icons/hicolor/scalable/apps/io.crisppdf.CrispPDF.svg"

rmdir "${data_home}/snapdf/app" 2>/dev/null || true

if "$purge"; then
  if ! "$assume_yes"; then
    if [[ ! -t 0 ]]; then
      echo "--purge needs an interactive terminal, or pass --yes explicitly." >&2
      exit 2
    fi
    printf '%s' "Erase all SnaPdf settings, annotations, chats, credentials, and OCR data? [y/N] "
    read -r confirmation
    [[ "$confirmation" == "y" || "$confirmation" == "Y" ]] || {
      echo "Personal data was kept."
      exit 0
    }
  fi

  rm -rf \
    "${config_home}/snapdf" \
    "${data_home}/snapdf" \
    "${config_home}/snappdf" \
    "${data_home}/snappdf" \
    "${config_home}/crisppdf" \
    "${data_home}/crisppdf"
  echo "SnaPdf and its saved user data were removed."
else
  rmdir "${data_home}/snapdf" 2>/dev/null || true
  echo "SnaPdf was removed. Saved projects, annotations, chats, settings, and OCR data were kept."
fi

command -v update-desktop-database >/dev/null 2>&1 \
  && update-desktop-database "${data_home}/applications" >/dev/null 2>&1 \
  || true
command -v gtk-update-icon-cache >/dev/null 2>&1 \
  && gtk-update-icon-cache -f -t "${data_home}/icons/hicolor" >/dev/null 2>&1 \
  || true

rm -f "${bin_dir}/snapdf-uninstall"
