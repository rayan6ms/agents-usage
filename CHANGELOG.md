# Changelog

## 0.3.1 — 2026-08-22

- Replaced the global 5-hour expansion option with an independent reset-counter preference that keeps countdowns visible without opening account details.
- Refined the Android connection screen with the canonical dark-and-white app icon, visible back navigation, neutral monochrome actions, a solid red icon-labeled Remove action, and clearer two-step pairing guidance.
- Improved first-run pairing language and automatic LAN/Tailscale route selection, and documented how to distinguish and remove obsolete browser shortcuts from the single native Android app.

## 0.3.0 — 2026-08-21

- Added an opt-in, token-protected phone view with the desktop's accounts, quota bars, countdowns, privacy choices, and Refresh action.
- Added a native Android 8+ companion with API-26-safe code, authenticated endpoint health probes, network-change failover, visible connection management, and automatic LAN/Tailscale pairing from one deep link.
- Added an in-app desktop setup wizard with hot enable/disable, safe loopback-by-default listening, explicit LAN access, address discovery, QR pairing, Tailscale Serve setup, status diagnostics, and per-phone revocation.
- Replaced the shared token with hashed ten-minute pairing credentials and independent, revocable phone sessions that do not require routine re-pairing; reverse-proxy cookies are path-scoped and forced refreshes are rate-limited.
- Added signed Android APKs and checksums to tagged GitHub releases, a signed workflow preflight, certificate verification, pinned Actions, Gradle checksum verification, desktop HTTP integration tests, and API 26/current Android emulator smoke tests.
- Added complete LAN, Tailscale Serve, updates, platform support, recovery, verification, troubleshooting, and security documentation.
- Unified setup and usage into one release app with matching visuals, and matched the desktop behavior by coloring only the reset countdown.

## 0.2.7 — 2026-08-20

- Reconcile moved Codex homes by normalized account email while preserving account settings and ordering.
- Ignore stale homes without current Codex authentication evidence and deduplicate account, cache, and refresh state.
- Fetch fresh usage only after the panel is opened or Refresh is requested, avoiding hidden startup traffic.
- Keep long account lists reachable with dashboard scrolling and add breathing room above Settings.
- Use the standard StatusNotifier tray fallback on non-GNOME Linux desktops.

## 0.2.6 — 2026-08-20

- Added account, remaining-quota gradient, and shared custom usage-bar color modes.
- Reduced the background startup refresh delay and made startup account checks concurrent.
- Stabilized reset countdown boundaries and removed redundant startup renders.
- Removed the ineffective global XInput outside-click listener.

## 0.2.5 — 2026-08-19

- Dismiss the Linux popup when clicking the bare desktop as well as another window.
- Render the compact OpenAI provider mark directly as a pixel-aligned vector path.
- Clarify first-run usage loading and report how many cached snapshots were restored.

## 0.2.4 — 2026-08-19

- Reduced multi-account refresh latency and restored click-away popup dismissal.
- Restored the last valid usage snapshot immediately at startup before refreshing it.
- Added account-name privacy, optional reset-timer colors, and a full custom color picker.
- Removed white from the preset palette and sharpened the compact OpenAI provider mark.

## 0.2.3 — 2026-08-17

- Reduced interactive refresh latency while keeping startup refresh sequential and delayed.
- Persisted account detail expansion and made the global 5-hour option expand every account.
- Refined settings sizing, color order, toggle styling, and public documentation.

## 0.2.2 — 2026-08-17

- Fixed hidden startup, first-open tray placement, fixed-height Settings scrolling, account ordering, stale-data errors, and reset persistence.
- Delayed background refresh by 10 seconds and bounded/coalesced Codex work.
- Added KDE, Windows, and macOS tray paths plus native release packaging.
- Added marker-based account discovery, portable Codex lookup, and optimized verified Linux packages.

## 0.2.1 — 2026-08-15

- Added account customization and ordering, hardened discovery and refresh errors, and verified Linux packages.

## 0.2.0 — 2026-08-15

- Added account settings, compact popup sizing, and reproducible Linux packaging.

## 0.1.0

- Initial OpenAI/Codex usage tray release.
