#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
# Keep local user and checkout paths out of distributed panic/debug strings.
separator=$(printf '\037')
if [ "${CARGO_ENCODED_RUSTFLAGS+x}" != x ]; then
    CARGO_ENCODED_RUSTFLAGS=''
    set -f
    for flag in ${RUSTFLAGS:-}; do
        CARGO_ENCODED_RUSTFLAGS="${CARGO_ENCODED_RUSTFLAGS}${CARGO_ENCODED_RUSTFLAGS:+$separator}$flag"
    done
    set +f
fi
export CARGO_ENCODED_RUSTFLAGS="${CARGO_ENCODED_RUSTFLAGS}${CARGO_ENCODED_RUSTFLAGS:+$separator}--remap-path-prefix=$HOME=/build${separator}--remap-path-prefix=$PWD=."
sh scripts/cargo.sh build --release --locked
app="dist/ssh-m8.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/ssh-m8 "$app/Contents/MacOS/ssh-m8.new"
mv "$app/Contents/MacOS/ssh-m8.new" "$app/Contents/MacOS/ssh-m8"
cp packaging/Info.plist "$app/Contents/Info.plist"
cp packaging/SSHM8.icns "$app/Contents/Resources/SSHM8.icns"
/usr/bin/codesign --force --sign - --identifier local.ssh-m8 "$app"
printf 'Built %s/dist/ssh-m8.app\n' "$PWD"
