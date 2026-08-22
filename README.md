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

Open desktop **Settings → Phone companion** and turn it on. New installations listen only on the desktop loopback interface, which is the safest default for Tailscale Serve. Turn on **Allow direct LAN** only if you want same-network access, then press **Pair a phone**. Agents Usage detects the routes you enabled and displays one short-lived QR code for them. Scan it with the phone camera and the Android app tests each authenticated address before opening the usage view. No desktop restart or terminal is normally required.

**Set up Tailscale** configures a private HTTPS path for access away from home. Tailscale must already be installed and signed into the same tailnet on both devices; an operating system may still request administrator approval. If explicitly enabled, direct LAN access uses TCP `3765`, so the desktop firewall may require one private-network approval. Never forward this port on a router and do not enable Tailscale Funnel.

Each phone receives an independent six-month session and can be revoked from desktop settings without disconnecting other phones. Pairing links expire after ten minutes, are valid only for the addresses included in that pairing operation, and are removed from the phone after use. Android health-checks the saved routes, switches between LAN and Tailscale after network changes, and exposes a visible Connections button.

Command-line controls remain available for recovery and scripted setups:

```bash
agents-usage --mobile-enable
agents-usage --mobile-pairing-url http://192.168.1.20:3765
agents-usage --mobile-pairing-url https://desktop.example.ts.net/agents-usage
agents-usage --mobile-rotate-token   # revokes every paired phone
agents-usage --mobile-disable
```

See the [complete phone setup and troubleshooting guide](docs/mobile-companion.md) for APK verification, system-specific firewall notes, CLI recovery, updates, connection management, and the full support/security model.

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

The Android companion additionally requires JDK 17 and the Android SDK. Its checked-in Gradle wrapper verifies the downloaded Gradle distribution:

```bash
cd mobile-android
./gradlew --no-daemon test lintDebug assembleDebug
```

Release APK signing is intentionally configured only through external environment variables; see [the release checklist](docs/releasing.md). The signing key and passwords must never be added to the repository.

## License

Agents Usage is licensed under [GPL-3.0-only](LICENSE). Third-party visual assets are listed in [THIRD_PARTY_ASSETS.md](THIRD_PARTY_ASSETS.md).
