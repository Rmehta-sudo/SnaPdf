#!/bin/sh
set -eu

REPOSITORY="Rmehta-sudo/Snapdf"
INSTALL_ROOT="${XDG_DATA_HOME:-$HOME/.local/share}/snappdf/app"
BIN_DIR="$HOME/.local/bin"

case "$(uname -m)" in
    x86_64|amd64) architecture="x86_64" ;;
    *)
        echo "SnapPDF's portable release currently supports x86_64 Linux desktops." >&2
        echo "For $(uname -m), build from source until a matching release is published." >&2
        exit 1
        ;;
esac

for command in curl sha256sum install mktemp mv; do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "Required command not found: $command" >&2
        exit 1
    fi
done

release_url="$(curl --proto '=https' --tlsv1.2 --fail --silent --show-error \
    --location --output /dev/null --write-out '%{url_effective}' \
    "https://github.com/$REPOSITORY/releases/latest")"
tag="${release_url##*/}"
version="${tag#v}"
asset="SnapPDF-${version}-${architecture}.AppImage"
download_root="https://github.com/$REPOSITORY/releases/download/$tag"

temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

echo "Downloading SnapPDF $version…"
curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location \
    "$download_root/$asset" --output "$temporary/$asset"
curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location \
    "$download_root/SHA256SUMS" --output "$temporary/SHA256SUMS"

expected="$(awk -v name="$asset" '$2 == name || $2 == "*" name { print; exit }' "$temporary/SHA256SUMS")"
if [ -z "$expected" ]; then
    echo "The release checksum does not contain $asset; refusing to install." >&2
    exit 1
fi
printf '%s\n' "$expected" > "$temporary/ASSET.SHA256"
(cd "$temporary" && sha256sum --check ASSET.SHA256)

install -d "$INSTALL_ROOT" "$BIN_DIR"
install -m 0755 "$temporary/$asset" "$INSTALL_ROOT/.SnapPDF.AppImage.new"
mv -f "$INSTALL_ROOT/.SnapPDF.AppImage.new" "$INSTALL_ROOT/SnapPDF.AppImage"

wrapper="$BIN_DIR/snappdf"
wrapper_new="$BIN_DIR/.snappdf.new"
cat > "$wrapper_new" <<EOF
#!/bin/sh
export APPIMAGE_EXTRACT_AND_RUN=1
export SNAPPDF_EXECUTABLE="$wrapper"
exec "$INSTALL_ROOT/SnapPDF.AppImage" "\$@"
EOF
chmod 0755 "$wrapper_new"
mv -f "$wrapper_new" "$wrapper"

"$wrapper" --register-only

echo
echo "SnapPDF $version is installed for this user."
echo "Open SnapPDF from the application menu, pin it to your dock, or run: snappdf"
echo "No system packages, administrator access, compiler, or Rust toolchain were used."
