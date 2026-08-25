# Agents Usage

Agents Usage is a small desktop tray app for checking the quota remaining in coding-agent subscriptions. It automatically discovers supported command-line tools and shows their provider-native quota periods in one compact view.

## Install

Download the package for your system from [GitHub Releases](https://github.com/rayan6ms/agents-usage/releases/latest). Install and sign in through at least one supported provider CLI; Agents Usage does not perform sign-in or sign-out itself.

- **Linux:** use the DEB, RPM, Flatpak, or AppImage. GNOME and KDE Plasma tray integrations are supported.
- **Windows:** extract the x86-64 ZIP and run `agents-usage.exe`.
- **macOS:** open the universal DMG or ZIP and launch Agents Usage. Apple Silicon and Intel Macs are supported.

Windows and macOS builds are not production-signed or notarized yet, so the operating system may ask you to confirm that you want to open them.

## Use

Launch Agents Usage, then click its tray icon to open the usage panel. The app discovers signed-in provider accounts automatically and validates each candidate before displaying it. Press Refresh after adding or changing a provider sign-in; there is no token-pasting or provider configuration step.

## Providers

| Provider | Automatic source | What Agents Usage shows |
| --- | --- | --- |
| OpenAI Codex | Codex CLI App Server | 5-hour/weekly quota, reset times, and available reset credits |
| OpenCode Go | OpenCode's local Go key and official Go usage API | 5-hour, weekly, and monthly quota |
| Anthropic Claude | Claude Code subscription sign-in | Session, weekly, and model-scoped limits returned for the account |
| Google Gemini | Gemini CLI Google sign-in and Code Assist quota API | Remaining quota and reset time for every returned model bucket |
| Cursor | Cursor Agent CLI sign-in and authenticated usage summary | Included, Auto, and API usage; individual and team-plan fallbacks |
| xAI Grok | Grok CLI sign-in and authenticated billing API | Included-credit usage and its weekly or monthly reset |

Quota capabilities are deliberately provider-specific. Agents Usage does not invent periods or derive quota from billing cost. See [provider support and troubleshooting](docs/providers.md) for the exact prerequisites, authenticated sources, and fallback behavior.

Settings let you rename, recolor, reorder, disable, and expand accounts. You can blur account names and emails, keep reset counters visible without expanding account details, choose preset or custom account colors, and optionally color reset timers from red toward green as the reset approaches. Usage bars can follow each account color, shift from green to red as quota is spent, or share one custom color.

Refreshes update accounts independently, preserve the last valid data when one account fails, and check for newly signed-in providers. The last valid usage is also cached locally so the panel has useful data immediately; a fresh check runs when the panel is opened or Refresh is pressed.

## Phone companion

Agents Usage can serve a phone-sized companion while keeping provider CLIs and account credentials on the desktop. The companion displays the same enabled accounts, provider icons, colors, quota windows, countdowns, and privacy choices. Its only desktop action is Refresh; settings and reset-credit actions are intentionally unavailable.

Download the **Agents Usage Android APK** from [GitHub Releases](https://github.com/rayan6ms/agents-usage/releases/latest). It supports Android 8.0 and newer and needs no account, root access, Termux, or USB connection.

Open desktop **Settings → Phone companion** and turn it on. New installations listen only on the desktop loopback interface, which is the safest default for Tailscale Serve. Turn on **Allow direct LAN** only if you want same-network access, then press **Show pairing QR**. Agents Usage detects the routes you enabled and displays one short-lived QR code for them. Scan it with the phone camera: the native app opens, pairs every available LAN/Tailscale route, tests them, and shows the first working view automatically. No desktop restart, account sign-in, terminal, or USB connection is normally required.

**Set up Tailscale** configures a private HTTPS path for access away from home. Tailscale must already be installed and signed into the same tailnet on both devices; an operating system may still request administrator approval. If explicitly enabled, direct LAN access uses TCP `3765`, so the desktop firewall may require one private-network approval. Never forward this port on a router and do not enable Tailscale Funnel.

Each phone receives an independent session that remains connected until it is revoked from desktop settings or its app data is cleared. Pairing links expire after ten minutes, are valid only for the addresses included in that pairing operation, and are removed from the phone after use. Android health-checks the saved routes, switches between LAN and Tailscale after network changes, and exposes visible Connections and Back controls. After one successful load, the last usage snapshot remains available when the desktop is asleep or disconnected and is clearly marked with its age.

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

- Cursor and Grok quota access depends on authenticated first-party endpoints used by their current clients; provider-side changes can require an adapter update.
- The app relies on existing sign-ins in the providers' official CLIs; it does not perform sign-in or sign-out.
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

GitHub Actions signs release APKs automatically; see [the release checklist](docs/releasing.md). Android requires a stable signing certificate for updates, but maintainers do not handle an encrypted key backup as part of a release.

## License

Agents Usage is licensed under [GPL-3.0-only](LICENSE). Third-party visual assets are listed in [THIRD_PARTY_ASSETS.md](THIRD_PARTY_ASSETS.md).
