# Agents Usage

Agents Usage is a small desktop tray app for checking the usage remaining on local OpenAI/Codex accounts. It asks each account's installed Codex App Server for current rate limits; it does not read or copy authentication tokens.

## Install

Download the package for your system from [GitHub Releases](https://github.com/rayan6ms/agents-usage/releases/latest). Codex must be installed, available on `PATH`, and already signed in.

- **Linux:** use the DEB, RPM, Flatpak, or AppImage. GNOME and KDE Plasma tray integrations are supported.
- **Windows:** extract the x86-64 ZIP and run `agents-usage.exe`.
- **macOS:** open the universal DMG or ZIP and launch Agents Usage. Apple Silicon and Intel Macs are supported.

Windows and macOS builds are not production-signed or notarized yet, so the operating system may ask you to confirm that you want to open them.

## Use

Launch Agents Usage, then click its tray icon to open the usage panel. The app discovers local Codex homes that contain account evidence and validates each candidate through Codex before displaying it.

Settings let you rename, recolor, reorder, disable, and expand accounts. You can blur account names and emails, choose preset or custom account colors, and optionally color reset timers from red toward green as the reset approaches. Usage bars can follow each account color, shift from green to red as quota is spent, or share one custom color.

Refreshes update accounts independently, preserve the last valid data when one account fails, and check for newly added Codex homes. The last valid usage is also cached locally so the panel has useful data immediately; a fresh check runs when the panel is opened or Refresh is pressed.

## Phone companion

Agents Usage can serve a phone-sized companion while keeping Codex and all account credentials on the desktop. The companion displays the same enabled accounts, colors, quotas, countdowns, and privacy choices. Its only desktop action is Refresh; settings and reset-credit actions are intentionally unavailable.

Download the **Agents Usage Android APK** from [GitHub Releases](https://github.com/rayan6ms/agents-usage/releases/latest). It supports Android 8.0 and newer and needs no account, root access, Termux, or USB connection.

Enable it once, then restart Agents Usage:

```bash
agents-usage --mobile-enable
```

The service listens on TCP port `3765` and requires a private 256-bit pairing token. Generate a pairing link using either the desktop's LAN address or a Tailscale HTTPS base URL:

```bash
agents-usage --mobile-pairing-url http://192.168.1.20:3765
agents-usage --mobile-pairing-url https://desktop.example.ts.net/agents-usage
```

Paste the generated link into the Android app and tap **Pair**. Add both a LAN link and a Tailscale link if desired; the app remembers only their non-secret base addresses and automatically tries another saved connection when one is unreachable.

For encrypted access at home or away, with no public port exposed, proxy the service through Tailscale Serve:

```bash
tailscale serve --bg --set-path /agents-usage http://127.0.0.1:3765
```

Direct LAN access works at `http://DESKTOP_LAN_IP:3765`, subject to the desktop firewall. Tailscale access works anywhere that both devices are connected to the same tailnet. Do not use port forwarding or Tailscale Funnel: this view is intended to remain private. Keep every pairing link private because anyone who has one and can reach the desktop can view usage and request a refresh.

To revoke paired phones, rotate the token and restart the app. To stop listening, disable mobile access and restart:

```bash
agents-usage --mobile-rotate-token
agents-usage --mobile-disable
```

See the [complete phone setup and troubleshooting guide](docs/mobile-companion.md), including LAN, Tailscale, APK verification, connection management, and the security model.

## Current limitations

- Remaining-quota reporting currently supports OpenAI accounts through Codex only.
- The app relies on the locally installed Codex App Server and an existing sign-in; it does not perform sign-in or sign-out.
- Linux tray placement is designed for GNOME and KDE Plasma. Other desktop environments may fall back to their standard StatusNotifier behavior.
- Windows and macOS packages are built and verified on native GitHub runners, but still need broader real-device testing.

## Build from source

Rust 1.92 or newer is required.

```bash
cargo test --locked
cargo build --release --locked
```

## License

Agents Usage is licensed under [GPL-3.0-only](LICENSE). Third-party visual assets are listed in [THIRD_PARTY_ASSETS.md](THIRD_PARTY_ASSETS.md).
