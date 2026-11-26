# DisplayAnything

Use your own computers together over your own network: see and control one computer's screen from another, share one keyboard and mouse between them, and copy on one and paste on the other. Windows and macOS today, with an iPhone and iPad viewer on the way.

- **Entirely local.** Your computers connect straight to each other by IP address. There is no account, no sign-in, no cloud server and no relay in the middle. The picture, your keystrokes and your clipboard never leave your network.
- **No internet needed.** Once it's installed, DisplayAnything never contacts the internet. It keeps working with the internet unplugged, as long as the computers can reach each other.
- **Works over a VPN.** If your VPN gives your devices private addresses (most home and office VPNs, such as WireGuard or OpenVPN, do), you can pair and connect across it just like on your home network. See [Using it over a VPN](#using-it-over-a-vpn) for the details.
- **Completely free.** No subscription, no trial, no paid tier, no ads. The code is open source under the [MIT licence](LICENSE): anyone may use, copy, change and share it, including commercially.

![The DisplayAnything window on a Windows PC, paired with a Mac mini](docs/images/tray-window.png)

*The main window (sample devices). It lives in the system tray (Windows) or the menu bar (Mac).*

## Download

**Latest release: [DisplayAnything 0.1.0](https://github.com/ZGremlin/DisplayAnything/releases/latest)** (build 4dbf18c), for Windows and macOS.

| Platform | Download |
| --- | --- |
| Windows | `DisplayAnything_0.1.0_x64-setup.exe` |
| macOS | `DisplayAnything-macOS.zip` and `DisplayAnythingInstall.sh` |
| Command-line agent (optional) | `da-agent.exe` (Windows), `da-agent-macos.tar.gz` (macOS) |

The builds aren't code-signed yet. On **Windows**, SmartScreen may warn the first time: click **More info**, then **Run anyway**. On a **Mac**, download both files, then run `bash ~/Downloads/DisplayAnythingInstall.sh ~/Downloads/DisplayAnything-macOS.zip` (see [Install](#install) for the full steps and the three [Mac permissions](#mac-permissions)). The iPhone and iPad app isn't in a release yet.

## What it does today

These features are merged and working on Windows and macOS, in either direction (a PC controlling a Mac, or a Mac controlling a PC).

### Remote screen control

Open another computer's screen in a window on this one, and use it with your own keyboard and mouse.

- **Control screen** next to a connected device opens its screen in a native window, using hardware H.264 encoding on the computer being viewed and hardware decoding on yours.
- **Monitor picker:** if the other computer has more than one monitor, numbered buttons (1, 2, …) pick which one you see, and switch an open window to it.
- **Picture size:** full resolution, match the screen you're watching on, or a fixed size from 4K (3840 x 2160) down to 1280 x 720. The remote picture is scaled to fit and keeps its shape. When you pick a size that isn't the other computer's own, it switches its display to that resolution for the session and puts it back afterwards.
- **Quality:** Highest (the default, about 80 Mbit/s), High, Medium or Low (about 8 Mbit/s) for slower networks.
- Both settings apply right away, including to windows that are already open, from the main window or from the viewer window's menu.
- Keys go to the other computer while its window is in front. **Ctrl+Alt+Esc** (Ctrl+Option+Esc on a Mac) ends control.
- The computer being controlled must allow it first: under **Saved devices**, tick "May control this desktop" for the computer that will view it (off by default). Its tray icon turns green while it's being controlled.
- Keyboard and mouse sharing pauses while a screen is being controlled, so the two never fight.

### Shared keyboard and mouse

One keyboard and mouse for two computers. Pick the other computer under **Keyboard and mouse**, then switch with a shortcut, the buttons in the window, or the tray menu.

| Shortcut (Windows) | Shortcut (Mac) | Does |
| --- | --- | --- |
| Ctrl+Alt+→ | Ctrl+Option+→ | Send keyboard and mouse to the other computer |
| Ctrl+Alt+← | Ctrl+Option+← | Bring them back |
| Ctrl+Alt+Esc | Ctrl+Option+Esc | Always bring them back, even if the link is broken |

- **Your own shortcuts:** under **Settings › Shortcuts**, click **Change** and hold any keys and/or mouse buttons (for example Ctrl + Mouse Back) for 2 seconds. Using the same combination for both directions makes it a toggle.
- **Edge crossing:** a switching mode of its own. Under **Settings › How to switch**, say which side the other computer is on, then push the pointer past that edge of your screen and carry on onto the other computer at the same height. It won't cross while you're dragging something, and Ctrl+Alt+L (changeable) pauses it for games or presenting. In this mode the switch shortcuts and buttons are off; the "always come back" shortcut still works.
- **Mac and PC keys:** on a Mac being used from a PC keyboard, Ctrl and the Windows key are swapped so Ctrl+C copies and Ctrl+Space opens Spotlight. Turn this off under **Settings › Keys** on the Mac.
- **Pointer speed** for the controlled computer is adjustable under **Settings › Keys**; display scaling is already accounted for.
- The tray icon turns green with a check mark on both computers while one is actually controlling the other.
- The receiving computer must allow it: "May control this computer with its keyboard and mouse" under **Saved devices** (on by default).

### Shared clipboard

When your keyboard and mouse move to the other computer, the clipboard goes with them, so you can copy on one and paste on the other.

- **Text**, **images** (up to 16 MB, sent as PNG) and **files** (up to 100 MB in total per copy) are supported.
- Copied files arrive in a temporary folder on the other computer and paste like normal files in Explorer or Finder. The last 5 batches are kept.
- The clipboard is shared through keyboard and mouse sharing, not through a remote screen window.

### Wake a sleeping Mac

Wake a sleeping Mac from another computer or your iPhone, then connect to it, without walking over to it. This uses Wake-on-LAN: the waking device sends a "magic packet" (a UDP broadcast on port 9) on your local network, and the Mac's network card wakes it up.

- **On the Mac, turn on "Wake for network access"** in **System Settings › Energy** (on a MacBook, **Battery › Options**). Older macOS calls it "Wake for Wi-Fi network access" or "Wake for Ethernet network access".
- **Keep the Mac plugged in.** Macs only wake for the network on power, not on battery.
- **Connect to it once while it's awake.** Each time a device pairs or connects, the Mac tells it the hardware (MAC) address of the network it's on; that is what the wake packet is addressed to. After updating, connect once with the new build on both sides.
- **Wake it:**
  - **Windows or Mac:** a **Wake** button appears next to a saved computer while it's offline. It reconnects on its own a few seconds after it wakes.
  - **iPhone or iPad:** swipe right on a computer (or touch and hold it) and tap **Wake**. If opening a computer fails, **Wake and connect** wakes it and keeps trying for about 45 seconds.
  - **Command line:** `da-agent wake <name>`, or `da-agent wake --mac a4:83:e7:0b:1c:2d`.
- **Same network only.** Wake packets don't cross routers or most VPNs, so this works on your home network, not from outside it.
- **Ethernet is the most reliable.** Waking over Wi-Fi depends on the Mac model.
- **On an iPhone or iPad,** iOS blocks broadcast packets for apps without a special Apple permission, so the app also sends the packet straight to the Mac's last address. That works when the phone talked to the Mac recently, or when an Apple TV or HomePod on the network is standing in for the sleeping Mac (Bonjour Sleep Proxy). If waking from the phone doesn't work but waking from another computer does, that's why.
- Windows PCs don't offer waking yet: they don't send a hardware address, so no Wake button appears for them.

### Pairing, tray and start-up

- **Pair once with a code:** type the other computer's IP address and click **Pair** (or **Find on network** to look for it). The other computer shows a 6-digit code; type it in, and you're done.
- **Saved devices reconnect on their own** whenever both are running, including after a reboot or an IP address change on the same network.
- **Start at login** is a checkbox at the top of the window.
- **Names:** click this computer's name to rename it. **Rename** next to a saved device sets what it's called on this computer only.
- **Tray menu:** open the window, switch the keyboard and mouse over or back, or quit.
- **Command-line agent** (`da-agent`) for headless machines, scripting and testing.

## How each mode works

DisplayAnything does one of two things between a pair of computers at any moment: share the keyboard and mouse, or control a screen. Starting remote screen control pauses keyboard and mouse sharing on both computers, and sharing picks up again when the screen window closes. **Ctrl+Alt+Esc** (Ctrl+Option+Esc on a Mac) always brings control back to the computer you're sitting at.

Every mode runs over a direct, encrypted connection between the two computers on TCP port 47800, and every mode needs the other computer's permission, set per device under **Saved devices** on the computer being controlled.

### Remote screen control

You see the other computer's screen in a window and work in it as if you were sitting there. Use it when the other computer has no monitor near you, or isn't in the room.

1. On the computer you'll control, under **Saved devices**, tick **May control this desktop** for the computer you'll sit at. It's off by default.
2. On the computer you're sitting at, click **Control screen** next to it. A window opens with its screen. If it has more than one monitor, the numbered buttons pick one.
3. While the window is in front, your keyboard and mouse work on the other computer. Click another window, or press Ctrl+Alt+Esc, to have them back. Close the window to end the session.

How it works: the controlled computer captures its screen (Windows Graphics Capture on Windows, ScreenCaptureKit on a Mac) and compresses it with the graphics card's hardware H.264 encoder. Your computer decodes it in hardware and draws it in a native window (Media Foundation and Direct3D 11 on Windows, AVFoundation on a Mac). Mouse positions are sent as points on the remote screen, so the pointer lands exactly where you click; keys are sent as they are. Picture size and quality are chosen under **Control screen picture** or from the window's menu. The controlled computer's tray icon turns green while it's being controlled.

### Shared keyboard and mouse

Both computers have their own monitors on your desk, and one keyboard and mouse drives whichever you choose. Nothing is streamed: you look at the other computer's own monitor.

1. On the computer that will receive input, under **Saved devices**, leave **May control this computer with its keyboard and mouse** ticked (on by default).
2. On the computer whose keyboard and mouse you're using, pick the other computer under **Keyboard and mouse**.
3. Switch over and back in one of two ways, chosen under **Settings › How to switch**:
   - **Shortcuts and buttons** (the default): Ctrl+Alt+→ sends the keyboard and mouse over, Ctrl+Alt+← brings them back. The buttons in the window and the tray menu do the same.
   - **Edge crossing:** say which side the other computer is on. Push the pointer past that edge of your screen and it carries on onto the other computer at the same height; push it back out the opposite edge to return. The switching shortcuts and buttons are off in this mode.

How it works: while the keyboard and mouse are over on the other computer, this computer stops acting on them and sends each key press, mouse move, click and scroll straight to the other computer, which plays them as if they came from its own keyboard and mouse. Key presses are sent as standard keyboard codes, so a PC keyboard works on a Mac and the other way round. Mouse movement is adjusted for each computer's display scaling. Both tray icons turn green with a check mark while one computer is controlling the other.

### Shared clipboard

Part of keyboard and mouse sharing; there's nothing to turn on. Each time the keyboard and mouse move between computers, whatever is on the clipboard goes with them: text, an image, or copied files (up to 100 MB). Copy on one computer, switch, and paste on the other. Copied files land in a temporary folder on the receiving computer and paste like normal files.

### iPhone and iPad

The phone or tablet is a remote screen window in your pocket: it pairs with a computer by IP address and code, and opens that computer's screen. The computer must tick **May control this desktop** for it. Milestone 1 is view-only; touch, pointer and keyboard control are being built. See [iPhone and iPad](#iphone-and-ipad-in-progress) below.

## iPhone and iPad (in progress)

A free iPhone and iPad app that shows and controls a computer's screen. It is a viewer only: it doesn't share keyboard and mouse or edge crossing.

**Not on `main` yet.** Milestone 1 is in pull request #6 and is being tested:

- Pair with a computer by typing its IP address and the 6-digit code it shows.
- View the computer's screen live.
- Pinch to zoom and drag to pan (on the device, with no delay), double-tap to reset.
- Saved computers are listed so you can reconnect with one tap. The computer saves the phone as view-only and never dials it on its own.

**Being built on the same branch:** touch control of the pointer (trackpad style by default, with a switch to direct touch), clicks and scrolling, and keyboard input including iPad hardware keyboards. These aren't finished, so they aren't described as working here.

The app needs iOS 17 or later. Until it's on the App Store, you build it yourself on a Mac with Xcode. A free Apple ID is enough, though Apple makes free-account installs expire after 7 days, so you reinstall it from Xcode once a week. The computer you view must have "May control this desktop" ticked for the phone.

## What's planned

- **Display takeover:** use another computer's monitor as an extra display for this one. Not built yet. (The "May take over my monitor" checkbox in the window is there for it, and does nothing today.)

## Install

Downloading needs the internet once; running DisplayAnything doesn't. Install it on every computer you want to use together.

### Windows

1. From the [latest release](https://github.com/ZGremlin/DisplayAnything/releases/latest), download `DisplayAnything_0.1.0_x64-setup.exe`.
2. Run it. The build isn't code-signed yet, so SmartScreen may say "Windows protected your PC": click **More info**, then **Run anyway**.
3. Open DisplayAnything from the Start menu. It sits in the system tray (bottom right; it may be under the **^** arrow). Click the icon to open its window.
4. The first time it runs, Windows asks whether to let it through the firewall. Allow it on **private networks**, so your other computers can reach it on TCP port 47800.
5. Optional: tick **Start at login** at the top of the window.

No extra permissions are needed on Windows. To control admin windows and UAC prompts on this PC from another computer, run DisplayAnything as administrator.

### macOS

No driver or paid Apple account is needed.

1. From the [latest release](https://github.com/ZGremlin/DisplayAnything/releases/latest), download `DisplayAnything-macOS.zip` and `DisplayAnythingInstall.sh` into your Downloads folder.
2. Open **Terminal** and run:

   ```sh
   bash ~/Downloads/DisplayAnythingInstall.sh ~/Downloads/DisplayAnything-macOS.zip
   ```

   The script quits any running copy, installs the app into Applications, removes the "downloaded from the internet" flag so macOS will open it, re-signs it locally, and clears any old permission grants. Then it opens DisplayAnything and the three System Settings pages below. (With no argument, it uses the newest `DisplayAnything-macOS` download in `~/Downloads`. The script is also in this repo at `scripts/DisplayAnythingInstall.sh`.)
3. Switch DisplayAnything **on** in each of the three permission pages (see the next section).
4. **Quit DisplayAnything from its menu-bar icon and open it again**, so macOS applies the new permissions.
5. Optional: tick **Start at login** at the top of the window.

Run the script again after installing each new build. macOS treats every new build as a new app, so the old permission grants silently stop working until you switch them on again.

### Mac permissions

DisplayAnything needs three permissions on a Mac. All three are in **System Settings › Privacy & Security**. Click the **+** if DisplayAnything isn't listed, and switch it on. If you can't find a page (on newer macOS versions, Accessibility isn't always where you'd expect), type its name into the System Settings search box.

| Permission | Where | What it's for | Without it |
| --- | --- | --- | --- |
| **Accessibility** | Privacy & Security › Accessibility | Watching this Mac's keyboard and mouse so they can go to another computer, and typing and clicking on this Mac when another computer controls it | Keyboard and mouse sharing doesn't start; the window shows a button that opens the prompt |
| **Input Monitoring** | Privacy & Security › Input Monitoring | Seeing key presses, so the keyboard (not just the mouse) can go to another computer | The mouse moves over but the keyboard doesn't: the usual "mouse works, keyboard doesn't" sign |
| **Screen Recording** | Privacy & Security › Screen Recording (called Screen & System Audio Recording on newer macOS) | Letting another computer see this Mac's screen in remote screen control | The other computer can't open this Mac's screen and gets an error naming this permission |

Which Mac needs which:

- **A Mac that controls another computer** (sharing its keyboard and mouse, or using a remote screen window): Accessibility and Input Monitoring. Without them, a remote screen window still opens but is view-only.
- **A Mac being controlled** (receiving shared keyboard and mouse, or being viewed): Accessibility, plus Screen Recording to be viewed.

The simplest thing is to switch on all three on every Mac.

### Test builds

Newer builds than the release are produced for every change by GitHub Actions (the **CI** workflow). Download `DisplayAnything-Windows` (an `.exe` installer plus `da-agent.exe`) or `DisplayAnything-macOS` (`DisplayAnything-macOS.zip` plus the agent) from a run's artifacts. The macOS artifact is a zip containing the zip; the install script handles that.

### Build it yourself

Needs Rust (stable) from https://rustup.rs. On Windows, also the "Desktop development with C++" workload from Visual Studio Build Tools.

```sh
cargo test --workspace          # run the tests
cargo run -p da-tray            # run the tray app
cargo run -p da-agent -- --help # command-line agent
```

For installers, install the Tauri CLI once with `cargo install tauri-cli --version "^2"`, then run `cargo tauri build` in `apps/tray/src-tauri`.

## Try it with two computers

1. Install and start DisplayAnything on both. They must be able to reach each other (same network, or a VPN as below), and TCP port 47800 must be allowed through the firewall.
2. On computer A, type computer B's IP address (shown in its own window, or use **Find on network**) and click **Pair**. Computer B shows a 6-digit code; type it on A.
3. That's it. They connect automatically from now on.

With the CLI: `da-agent serve` on B, `da-agent pair 192.168.1.20` on A, then `da-agent connect 192.168.1.20` to check the link and its latency.

## Local only, and no internet

DisplayAnything is built to run on your network and nowhere else.

- The app connects only to the IP addresses of your own paired computers, on TCP port 47800. It contains no update checks, analytics, crash reporting, licence checks or cloud services, and its window loads nothing from the web (its content security policy allows only the app's own files).
- **Find on network** and automatic reconnection after an IP change use mDNS (Bonjour), which only talks to devices on your local network.
- **Wake** sends a Wake-on-LAN packet (UDP port 9) as a broadcast on your local network, and to the sleeping computer's last private address. Nothing else.
- Connections from addresses that aren't private (anything that could come from the internet) are refused by default, so even a router port-forward can't expose it.
- Everything a computer knows about its paired devices is stored on that computer.

## Using it over a VPN

DisplayAnything doesn't care how two computers reach each other, only that the address is private. It accepts the private address ranges:

- IPv4: `10.x.x.x`, `172.16.x.x` to `172.31.x.x`, `192.168.x.x`, plus link-local `169.254.x.x`
- IPv6: unique local `fc00::/7` and link-local `fe80::/10`

So over a VPN that hands out addresses in those ranges (WireGuard and OpenVPN setups, most router and office VPNs, ZeroTier's usual ranges), you pair and connect exactly as at home: use the other computer's VPN address. Things to know:

- **Find on network won't see across most VPNs**, because mDNS stays on the local network. Type the VPN address instead. The app remembers it and reconnects on its own.
- **Tailscale doesn't work out of the box.** Tailscale's IPv4 addresses (`100.64.x.x` to `100.127.x.x`) aren't in the private ranges, so the app refuses them, and the desktop app currently listens on IPv4 only, so Tailscale's IPv6 addresses can't be used either. The command-line agent can accept them with `da-agent serve --allow-public`, but the tray app has no such setting yet.
- **iPhone and iPad:** the app follows the same rule. On your home Wi-Fi it reaches your computers directly. Away from home, turn on a VPN on the phone (for example the WireGuard app) that gives it a private address, and type the computer's VPN address. iOS asks once for Local Network access; allow it.
- Over a slower VPN link, lower **Quality** and the picture size in **Control screen picture**.

## Security

Every connection is mutual TLS 1.3 with certificates pinned when you pair. Pairing uses SPAKE2 bound to the TLS session, so the code can't be guessed offline, and pairing locks for 10 minutes after 5 wrong codes. Remote screen control is off for each device until you allow it.

## Troubleshooting

The app keeps a log (`da.log` in its data folder; **Show log file** at the bottom of the window), which you can switch off under **Settings › Troubleshooting**. The window footer shows the build, so you can check both computers run the same one.

Known limits:

- Windows won't let a normal app send input to admin windows or UAC prompts. Run DisplayAnything as administrator if you need that.
- On Windows, while one of DisplayAnything's own windows is in front, Windows hands keys straight to it and the keyboard hook can't see them. The app moves focus to the taskbar when you switch over, and a watchdog reinstalls the hook if it misses keys, but shortcuts still don't fire while you're typing in its window.

## Layout

| Path | What it is |
| --- | --- |
| `crates/da-core` | Identity, pairing, trust store, TLS connections, discovery, auto-reconnect, remote control sessions, Wake-on-LAN |
| `crates/da-input` | Keyboard and mouse capture and injection (Windows, macOS), key mapping, switching, clipboard |
| `crates/da-screen` | Screen capture, hardware H.264 encode and decode, the native viewer window |
| `crates/da-agent` | Command-line agent |
| `apps/tray` | Tauri tray app: `src-tauri` (Rust) and `ui` (plain HTML and JS, no build step) |
| `scripts/DisplayAnythingInstall.sh` | macOS install and permissions helper |

## Licence

MIT. See [LICENSE](LICENSE).

The iPhone and iPad app (`apps/ios`, `crates/da-mobile`) arrives with pull request #6.
