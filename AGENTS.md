# ssh-m8

For managing the installed app, run `ssh-m8 agent-help` or read [docs/agents.md](docs/agents.md). Use the CLI for hosts, tunnels, status, and logs. It controls the same library and connections as the window. Do not edit live library files or extract stored credentials.

For source changes, keep the app small and event-driven. Reuse the existing validation, atomic saves, Keychain handling, and tunnel worker. Avoid idle polling and new dependencies for simple platform features.

Run `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, and `cargo test --locked`. Build the Mac bundle with `sh scripts/build-app.sh`. Use an isolated `SSH_M8_DATA_DIR` for app/CLI integration checks; never mutate the normal library during tests. See [CONTRIBUTING.md](CONTRIBUTING.md).

Use fictional hosts and loopback addresses in fixtures and screenshots. Never commit saved libraries, credentials, real hostnames, local machine details, or captured user logs.
