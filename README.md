# DisplayAnything

Share displays, keyboard and mouse between your own computers over the local network, by IP. No accounts, no cloud relay.

- **Remote desktop:** view and control another machine on your LAN.
- **Display takeover:** use another machine's monitor as an extra display for this one.
- **Input takeover:** one keyboard and mouse for two machines, switched with a hotkey.

Design doc: https://claude.ai/artifact/SqBs1JcqAcTA3o4Rhp4xiz

## Status

- Milestone 1: device identity, pairing with a 6-digit code, encrypted connections by IP, saved devices that reconnect on their own, start at login, tray app.
- Milestone 2: keyboard and mouse sharing between two paired machines, switched with a hotkey; clipboard (text) follows.
- Next: remote desktop (milestone 3) and display takeover (milestone 4).

## Layout

| Path | What it is |
| --- | --- |
| `crates/da-core` | Identity, pairing, trust store, TLS connections, discovery, auto-reconnect |
| `crates/da-input` | Keyboard/mouse capture and injection (Windows, macOS), key mapping, switching logic |
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

**Names:** click this computer's name at the top of the window to rename it; connected devices pick up the new name within a few seconds. **Rename** next to a saved device sets what it's called on this computer only (clear it to go back to that device's own name). CLI: `da-agent --name "Studio Mac" serve`.

With the CLI: `da-agent serve` on B, then `da-agent pair 192.168.1.20` on A, then `da-agent connect 192.168.1.20` to check the link and latency.

## Keyboard and mouse sharing

On the computer whose keyboard and mouse you want to use, open the tray window and pick the other device under **Keyboard and mouse**. Then:

| Shortcut (Windows) | Shortcut (Mac) | Does |
| --- | --- | --- |
| Ctrl+Alt+→ | Ctrl+Option+→ | Send keyboard and mouse to the other computer |
| Ctrl+Alt+← | Ctrl+Option+← | Bring them back |
| Ctrl+Alt+Esc | Ctrl+Option+Esc | Always bring them back, even if the link is broken |

The tray menu has the same switch. When input moves, the clipboard text moves with it. On a Mac receiving input from a PC keyboard, Ctrl and the Windows key are swapped so Ctrl+C copies.

The receiving computer must allow it: under **Saved devices**, "May control this computer with its keyboard and mouse" (on by default).

CLI: `da-agent serve --share-input-with "Mac mini"`.

Known limits: Windows won't let a normal app send input to admin windows or UAC prompts; run the app as administrator if you need that.

## macOS notes

No driver or paid Apple account is needed. After installing or updating a build, run `bash scripts/macos-setup.sh ~/Downloads/DisplayAnything-macOS.zip`: it installs the app, clears the quarantine flag, re-signs it locally and resets the permission grants that a new build invalidates, then opens the prompt. Keyboard and mouse sharing needs the **Accessibility** permission (System Settings › Privacy & Security › Accessibility); the app shows a button that opens the prompt. Screen sharing (later) will also need Screen Recording.

## Security

Every connection is mutual TLS 1.3 with certificates pinned at pairing. Pairing uses SPAKE2 bound to the TLS session, so the code can't be brute-forced offline, and pairing locks for 10 minutes after 5 wrong codes. Connections from non-private IP addresses are refused by default. See the design doc for the full threat model.
