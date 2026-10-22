#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
if [ "$#" -gt 1 ]; then
    printf 'Usage: sh scripts/package-release.sh [https-release-directory-url]\n' >&2
    exit 1
fi
release_url=${1:-}
if [ -n "$release_url" ]; then
    case "$release_url" in https://?*) ;; *) printf 'Release URL must use HTTPS.\n' >&2; exit 1 ;; esac
    case "$release_url" in *[!a-zA-Z0-9_./:-]*) printf 'Release URL contains unsupported characters.\n' >&2; exit 1 ;; esac
    release_url=${release_url%/}
fi
sh scripts/build-app.sh
app=dist/ssh-m8.app
version=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist")
arch=$(/usr/bin/lipo -archs "$app/Contents/MacOS/ssh-m8")
case "$arch" in arm64|x86_64) ;; *) printf 'Expected a single arm64 or x86_64 build.\n' >&2; exit 1 ;; esac
archive="ssh-m8-$version-macos-$arch.zip"
output="dist/release/$version/$arch"
mkdir -p "$output"
rm -f "$output/install-$arch.sh"
/usr/bin/codesign --verify --deep --strict "$app"
/usr/bin/ditto -c -k --keepParent --zlibCompressionLevel 9 "$app" "$output/$archive"
checksum=$(shasum -a 256 "$output/$archive" | cut -d ' ' -f 1)
printf '%s  %s\n' "$checksum" "$archive" > "$output/SHA256SUMS"
printf 'Release archive: %s/%s\n' "$output" "$archive"
if [ -n "$release_url" ]; then
    sed -e "s|@RELEASE_URL@|$release_url|g" -e "s|@ARCHIVE@|$archive|g" \
        -e "s|@SHA256@|$checksum|g" -e "s|@ARCH@|$arch|g" \
        packaging/install.sh.in > "$output/install-$arch.sh"
    printf 'Upload the archive and install-%s.sh to %s\n' "$arch" "$release_url"
    printf "Install command: curl -fsSL '%s/install-%s.sh' | sh\n" "$release_url" "$arch"
else
    printf 'Pass the public HTTPS release directory URL to also generate a one-line download installer.\n'
fi
