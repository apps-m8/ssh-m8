#!/usr/bin/env python3
"""Check the release installer in disposable folders without launching the app."""
import hashlib
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tempfile

if len(sys.argv) != 3:
    raise SystemExit("Usage: python3 scripts/check-installer.py RELEASE.zip INSTALLER.sh")
archive = Path(sys.argv[1]).resolve()
template = Path(sys.argv[2]).read_text()
release_url = re.search(r"^    release_url='([^']+)'$", template, re.MULTILINE).group(1)
checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
assert f"checksum='{checksum}'" in template
assert f"archive='{archive.name}'" in template
assert f"architecture='{platform.machine()}'" in template

with tempfile.TemporaryDirectory(prefix="ssh-m8-installer-") as temporary:
    root = Path(temporary)
    mocks = root / "mocks"
    mocks.mkdir()
    # Only transport, process discovery, and an injected link failure are faked.
    # Extraction, checksums, signatures, quarantine, copying, and rollback are real.
    curl = mocks / "curl"
    curl.write_text("""#!/bin/sh
set -eu
[ "$*" = "--fail --silent --show-error --location --proto =https --proto-redir =https --tlsv1.2 $SSH_M8_TEST_URL -o ${12}" ]
case "${SSH_M8_TEST_DOWNLOAD:-ok}" in
    fail) exit 22 ;;
    corrupt) printf 'broken download' > "${12}" ;;
    *) cp "$SSH_M8_TEST_ARCHIVE" "${12}" ;;
esac
""")
    pgrep = mocks / "pgrep"
    pgrep.write_text('#!/bin/sh\n[ "${SSH_M8_TEST_RUNNING:-false}" = true ]\n')
    ln = mocks / "ln"
    ln.write_text('''#!/bin/sh
[ "${SSH_M8_TEST_LINK_FAIL:-false}" != true ] || exit 1
/bin/ln "$@" || exit 1
if [ "${SSH_M8_TEST_LINK_INTERRUPT:-false}" = true ]; then
    kill -TERM "$PPID"
fi
''')
    for command in (curl, pgrep, ln):
        command.chmod(0o755)
    environment = {
        **os.environ,
        "PATH": f"{mocks}:/usr/bin:/bin:/usr/sbin:/sbin",
        "SSH_M8_DATA_DIR": str(root / "library"),
        "SSH_M8_TEST_ARCHIVE": str(archive),
        "SSH_M8_TEST_URL": f"{release_url}/{archive.name}",
    }

    def install(name, script=template, expected=0, **overrides):
        destination = root / name
        result = subprocess.run(
            ["/bin/sh", "-s", "--", str(destination / "Applications"), str(destination / "bin")],
            input=script, text=True, capture_output=True,
            env={**environment, **overrides}, timeout=30,
        )
        assert result.returncode == expected, (result.returncode, result.stdout, result.stderr)
        assert not list((destination / "Applications").glob(".ssh-m8-install.*"))
        return result, destination / "Applications/ssh-m8.app", destination / "bin/ssh-m8"

    _, app, link = install("Fresh install")
    assert link.is_symlink() and link.resolve() == (app / "Contents/MacOS/ssh-m8").resolve()
    assert subprocess.check_output(["/usr/bin/xattr", "-p", "com.apple.quarantine", str(app)]).startswith(b"0083;")
    subprocess.run(["/usr/bin/codesign", "--verify", "--deep", "--strict", str(app)], check=True)
    binary_digest = hashlib.sha256(link.read_bytes()).hexdigest()
    assert os.fsencode(Path.home()) not in link.read_bytes(), "Release contains the builder's home directory"
    assert not (root / "library").exists(), "Installer must not open or change a library"

    # Failed downloads and updates preserve the existing installation.
    for mode in ("fail", "corrupt"):
        result, _, _ = install("Fresh install", expected=22 if mode == "fail" else 1, SSH_M8_TEST_DOWNLOAD=mode)
        assert hashlib.sha256(link.read_bytes()).hexdigest() == binary_digest
        if mode == "corrupt":
            assert "checksum mismatch" in result.stderr
    result, _, _ = install("Fresh install", expected=1, SSH_M8_TEST_RUNNING="true")
    assert "Quit ssh-m8" in result.stderr
    install("Fresh install")
    assert hashlib.sha256(link.read_bytes()).hexdigest() == binary_digest

    # A CLI install failure restores the old app and cleans a fresh install.
    link.unlink()
    marker = app / "previous-install-marker"
    marker.write_text("keep me")
    install("Fresh install", expected=1, SSH_M8_TEST_LINK_FAIL="true")
    assert marker.read_text() == "keep me" and not link.exists()
    _, fresh_app, _ = install("Failed fresh", expected=1, SSH_M8_TEST_LINK_FAIL="true")
    assert not fresh_app.exists()
    _, interrupted_app, interrupted_link = install("Interrupted fresh", expected=1, SSH_M8_TEST_LINK_INTERRUPT="true")
    assert not interrupted_app.exists() and not interrupted_link.is_symlink()
    install("Fresh install", expected=1, SSH_M8_TEST_LINK_INTERRUPT="true")
    assert marker.read_text() == "keep me" and not link.is_symlink()

    unrelated = root / "Unrelated/Applications/ssh-m8.app"
    unrelated.parent.mkdir(parents=True)
    unrelated.write_text("keep me")
    result, _, _ = install("Unrelated", expected=1)
    assert "unrelated item" in result.stderr and unrelated.read_text() == "keep me"
    unrelated_link = root / "CLI conflict/bin/ssh-m8"
    unrelated_link.parent.mkdir(parents=True)
    unrelated_link.write_text("keep me")
    result, conflict_app, _ = install("CLI conflict", expected=1)
    assert "unrelated CLI" in result.stderr and unrelated_link.read_text() == "keep me"
    assert not conflict_app.exists()

    wrong_arch = template.replace(f"architecture='{platform.machine()}'", "architecture='unsupported'")
    result, wrong_app, _ = install("Wrong architecture", script=wrong_arch, expected=1)
    assert "architecture" in result.stderr and not wrong_app.exists()
    # A pipe cut off before the final invocation must not install anything.
    truncated = template.rsplit('install_ssh_m8 "$@"', 1)[0]
    _, truncated_app, _ = install("Truncated download", script=truncated)
    assert not truncated_app.exists()

print("Installer checks passed: install, update, checksum, quarantine, conflicts, rollback, and partial-script safety.")
