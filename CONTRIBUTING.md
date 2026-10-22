# Contributing

Keep ssh-m8 small: saved SSH hosts, reliable tunnels, useful logs, and a native macOS interface.

Before sending a change:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
sh scripts/build-app.sh
```

Use `SSH_M8_DATA_DIR` with a temporary directory when testing storage or connection settings. Use loopback or `example.org` in fixtures and screenshots. Do not commit your library, SSH configuration, keys, credentials, terminal output, or real hostnames.

For connection changes, run the ignored loopback tests described in the README. Include what changed, how it was tested, and any effect on idle CPU, memory, or connection behavior. A small fix with a focused regression test is welcome.

For CLI changes, launch the app with an empty, isolated `SSH_M8_DATA_DIR`, then run `python3 scripts/check-cli.py path/to/ssh-m8` with that same environment variable. The check creates and removes fictional entries and uses only loopback connection attempts. Add `--keychain` to exercise a disposable saved password too. It refuses the normal Application Support directory and nonempty libraries. Test with the window minimized as well as visible.

For a possible security issue, use the repository's private vulnerability reporting feature if enabled. Otherwise ask for a private reporting channel without including credentials or exploit details in a public issue.

## Packaging a release

Build a compressed app archive and checksum file:

```sh
sh scripts/package-release.sh
```

To generate the download installer too, pass the public HTTPS directory where the release assets will be uploaded. This is an example; replace the repository and version first:

```sh
sh scripts/package-release.sh https://github.com/apps-m8/ssh-m8/releases/download/v0.1.0
```

Outputs go to `dist/release/VERSION/ARCH/`: the app ZIP, `SHA256SUMS`, and `install-ARCH.sh`. The installer embeds that archive's checksum and URL. Upload the installer and ZIP together to the supplied release location, plus the checksum file. Do not replace a published ZIP in place: publish a new version and regenerate its installer. Update `Cargo.toml` and the versions in `packaging/Info.plist` together.

Packaging uses the build machine's architecture. Build and test on Apple Silicon for `arm64`, or Intel for `x86_64`; do not advertise an architecture until its release has been tested. The current build script uses ad-hoc signing. Public releases remain subject to macOS first-open approval until Developer ID signing and notarisation are added.

Run the installer check with the generated archive and script. For a local packaging check before a public URL exists, use `https://example.org/releases/test` as the release URL; the check fakes the download and never contacts that address.

```sh
python3 scripts/check-installer.py \
  dist/release/0.1.0/arm64/ssh-m8-0.1.0-macos-arm64.zip \
  dist/release/0.1.0/arm64/install-arm64.sh
```

It uses disposable directories and fakes only the download, process discovery, and an injected CLI-link failure. It checks real extraction, checksums, signatures, quarantine, install/update behavior, conflicts, and rollback without opening the app or accessing a saved library. Also verify the download and first-open experience on a clean Mac before publishing.

For a manual isolated install, download the generated script and pass two absolute destination directories:

```sh
sh install-arm64.sh /absolute/test/Applications /absolute/test/bin
```

The installer does not modify shell profiles, create a login item, open the app, or read Application Support or Keychain. Existing unrelated CLI links are left alone; users must resolve any conflict explicitly.
