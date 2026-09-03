# Changelog

## 0.4.6 — 2026-09-03

- Report missing, unreadable, expired, or invalid provider credentials separately from genuine rate limits, with actionable recovery guidance.
- Distinguish provider timeouts, connection failures, malformed responses, and temporary server errors while preserving the last valid usage snapshot.
- Keep saved accounts visible when credentials disappear so their authentication failure can be explained instead of silently dropping the account.
- Refresh the phone companion more quickly with adaptive polling, shorter background checks, desktop-equivalent freshness, accessible error announcements, and a new service-worker cache.

## 0.4.5 — 2026-08-30

- Added provider-supplied plan badges to desktop and phone account rows with a setting to hide them.
- Improved account controls, layout behavior, and Codex authentication feedback.

## 0.4.4 — 2026-08-26

- Made multi-route phone pairing succeed when either LAN or Tailscale works instead of reporting a global error after one route has already paired.
- Made pairing retries skip saved routes whose authenticated health check already passes, preserving the remaining one-time token use for an unpaired route.
- Added actionable Android Tailscale DNS guidance and made desktop Tailscale Serve setup non-interactive, verified, and advertised only when it targets the active companion port.

## 0.4.3 — 2026-08-25

- Kept very dark account colors legible in quota bars on desktop and phone while preserving the chosen color for account indicators.
- Replaced the ambiguous custom-color swatch with a distinct spectrum control and kept the account color picker open while selecting hue, saturation, and brightness.
- Gave Android installations stable identities, consolidated historical duplicate phone entries into the most recently used record, and preserved their working LAN and Tailscale sessions.
- Added dashboard, settings, and account-color screenshots to the README.

## 0.4.2 — 2026-08-25

- Added an option to hide banked resets across desktop and phone views, while showing their exact local expiration time when visible.
- Kept the dashboard scrollbar at a steady width on hover and aligned settings switches directly with their labels.
- Added concise contribution guidance covering regression checks and disclosure of AI-agent assistance.

## 0.4.1 — 2026-08-23

- Kept the Android companion's last successful usage snapshot available while every desktop route is offline, with an explicit age indicator, advancing reset countdowns, and working Back navigation from Connections.
- Added breathing room above Android connection errors and cleared cached account data when an endpoint is removed or explicitly rejects a revoked session.
- Added live Cursor Included/Auto/API bars with individual and team-plan fallbacks, plus Grok included-credit bars with provider-returned weekly or monthly periods.
- Hardened provider compatibility for Claude's mixed core/scoped limits, Gemini's same-model token buckets, OpenCode field aliases, expired sessions, rate limits, and deterministic Grok credential selection.

## 0.4.0 — 2026-08-22

- Added automatic discovery and live quota adapters for OpenCode Go, Anthropic Claude, and Google Gemini alongside OpenAI Codex.
- Added authenticated Cursor and xAI Grok discovery with honest capability notices while their individual consumer usage remains unavailable through supported APIs.
- Preserved every provider-specific quota window, including OpenCode monthly and Claude/Gemini scoped limits, across the desktop and phone views.
- Added current Gemini keychain, encrypted-file, and legacy OAuth credential compatibility; provider-aware settings/cache identities; bounded provider requests; actionable first-refresh failures; and stale-data fallback.
- Added the complete provider icon set and provider-specific marks to both desktop and mobile.

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
