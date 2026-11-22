#!/usr/bin/env bash
set -euo pipefail

image=${1:?Usage: macos_dmg_smoke.sh <disk-image> <release-tag>}
tag=${2:?Usage: macos_dmg_smoke.sh <disk-image> <release-tag>}

hdiutil verify "$image"
mount_point=$(mktemp -d)
mounted=0
cleanup() {
    if (( mounted )); then
        hdiutil detach "$mount_point"
    fi
    rmdir "$mount_point"
}
trap cleanup EXIT

hdiutil attach -readonly -nobrowse -mountpoint "$mount_point" "$image"
mounted=1

app="$mount_point/Arcnix.app"
executable="$app/Contents/MacOS/arcnix"
[[ $(readlink "$mount_point/Applications") == /Applications ]]
[[ -f "$mount_point/LICENSE" && -x "$executable" ]]
codesign --verify --strict "$app"
[[ $("$executable" --version) == "arcnix ${tag#v}" ]]
