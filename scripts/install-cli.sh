#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
app=${1:-dist/ssh-m8.app}
destination=${2:-"$HOME/.local/bin"}
binary=$(cd "$app/Contents/MacOS" && pwd -P)/ssh-m8
if [ ! -x "$binary" ]; then
    printf 'Build ssh-m8 first with: sh scripts/build-app.sh\n' >&2
    exit 1
fi
mkdir -p "$destination"
link="$destination/ssh-m8"
if [ -e "$link" ] && [ ! -L "$link" ]; then
    printf 'Refusing to replace an existing file: %s\n' "$link" >&2
    exit 1
fi
ln -sfn "$binary" "$link"
printf 'Installed %s\nEnsure %s is on your PATH.\n' "$link" "$destination"
