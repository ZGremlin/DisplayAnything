# DisplayAnything

Share displays, keyboard and mouse between your own computers over the local network, by IP. No accounts, no cloud relay.

- **Remote desktop:** view and control another machine on your LAN.
- **Display takeover:** use another machine's monitor as an extra display for this one.
- **Input takeover:** one keyboard and mouse for two machines, switched with a hotkey.

Design doc: https://claude.ai/artifact/SqBs1JcqAcTA3o4Rhp4xiz

## Status

Milestone 1 (this code): device identity, pairing with a 6-digit code, encrypted connections by IP, saved devices that reconnect on their own, start at login, and a tray app. Screen and input sharing come in milestones 2 to 4.

## Layout

| Path | What it is |
| --- | --- |
| `crates/da-core` | Identity, pairing, trust store, TLS connections, discovery, auto-reconnect |
| `crates/da-agent` | Command-line agent (headless machines, scripting, testing) |
| `apps/tray` | Tauri tray app: `src-tauri` (Rust) and `ui` (plain HTML/JS, no build step) |

## Build

Needs Rust (stable) from https://rustup.rs. On Windows, also the "Desktop development with C++" workload from Visual Studio Build Tools.

```sh
cargo test --workspace          # run the tests
cargo run -p da-tray            # run the tray app
cargo run -p da-agent -- --help # command-line agent
```

To build installers (`.msi`/`.exe` on Windows, `.app`/`.dmg` on macOS), install the Tauri CLI once with `cargo install tauri-cli --version "^2"`, then run `cargo tauri build` from `apps/tray/src-tauri`.

## Try it with two machines

Both machines must be on the same network, and TCP port 47800 must be allowed through the firewall (Windows asks the first time).

1. Start the tray app (or `da-agent serve`) on both.
2. On machine A, type machine B's IP address and click **Pair**. Machine B shows a 6-digit code; type it on A.
3. That's it. From now on the two connect automatically whenever both are running, including after a reboot or an IP change.

With the CLI: `da-agent serve` on B, then `da-agent pair 192.168.1.20` on A, then `da-agent connect 192.168.1.20` to check the link and latency.

## macOS notes

No driver or paid Apple account is needed. Later milestones need Screen Recording, Accessibility and Input Monitoring permissions, granted once in System Settings › Privacy & Security.

## Security

Every connection is mutual TLS 1.3 with certificates pinned at pairing. Pairing uses SPAKE2 bound to the TLS session, so the code can't be brute-forced offline, and pairing locks for 10 minutes after 5 wrong codes. Connections from non-private IP addresses are refused by default. See the design doc for the full threat model.
