# Phone companion setup

The Agents Usage phone companion is a read-only view of the usage information already collected by the desktop app. Codex remains installed and signed in only on the desktop. The Android app does not contain a provider sign-in, account-management controls, reset-credit controls, or desktop file access.

## What you need

- Agents Usage 0.3.0 or newer running on the desktop.
- Android 8.0 or newer.
- For LAN access, both devices on the same private network.
- For remote private access, Tailscale on both devices and both signed into the same tailnet.

Root, ADB, Termux, a permanent USB connection, and a public server are not required.

## 1. Install the Android app

1. Open the [latest Agents Usage release](https://github.com/rayan6ms/agents-usage/releases/latest) on the phone.
2. Download `Agents-Usage-VERSION-android.apk` and its adjacent `.sha256` file.
3. If desired, verify the APK with a checksum utility. Its SHA-256 value must match the line in the `.sha256` file.
4. Open the APK. Android may ask you to allow installs from the browser or file manager because the app is distributed directly through GitHub.

Future GitHub APKs use the same release signing key, so Android can install them as updates without losing saved connections.

The official Android signing-certificate SHA-256 fingerprint is:

```text
c9a8159f30df08f5b5613ea0b438d4746c292f600aca3bc9ab48f5c5d7a540bf
```

## 2. Enable the desktop service

Run:

```bash
agents-usage --mobile-enable
```

Quit and reopen Agents Usage once. The companion service listens on TCP port `3765` by default and requires a private 256-bit pairing token. It does not expose Codex credentials, local paths, settings, or reset actions.

## 3A. Pair over the LAN

Find the desktop's private LAN address. It commonly starts with `192.168.`, `10.`, or `172.16` through `172.31`. Then run, replacing the example address:

```bash
agents-usage --mobile-pairing-url http://192.168.1.20:3765
```

Copy the resulting private link to the phone, paste it into the Android app, and tap **Pair**. The desktop firewall must allow inbound TCP `3765` from the private network. Do not forward this port on the router.

Plain HTTP is intentionally accepted by the Android app only for loopback, private LAN, link-local, `.local`, and Tailscale CGNAT addresses. A public IP or ordinary hostname must use HTTPS.

## 3B. Pair through Tailscale

Tailscale is the recommended option for access away from home. It provides an encrypted path and does not expose the companion to the public internet.

On the desktop, publish the local companion at a dedicated tailnet-only path:

```bash
tailscale serve --bg --set-path /agents-usage http://127.0.0.1:3765
tailscale serve status
```

Use the HTTPS hostname shown by Tailscale to generate a second pairing link:

```bash
agents-usage --mobile-pairing-url https://DESKTOP.TAILNET.ts.net/agents-usage
```

Paste that link into the Android app and tap **Pair**. Tailscale must be connected on both devices. Tailscale Serve is sufficient; do not enable Tailscale Funnel.

If another service already uses the root Tailscale Serve path, `--set-path /agents-usage` leaves it in place.

## Managing connections

The app can save both LAN and Tailscale base addresses. It tries the last working address first and falls back to another saved address after a main-page connection failure.

- Press Android **Back** while viewing usage to open **Saved connections**.
- Tap **Use** to prefer a connection immediately.
- Tap **Remove** to forget an address on the phone.
- Generate and pair a fresh link if the desktop token was rotated.

The pairing URL itself is not kept in the saved-connections list. Authentication is retained as an HttpOnly, same-site cookie scoped to that desktop origin, so the rendered page cannot read it.

## Revoking access

To invalidate every paired browser and phone, rotate the desktop token and restart Agents Usage:

```bash
agents-usage --mobile-rotate-token
```

Pair trusted phones again with newly generated links. To turn the service off entirely:

```bash
agents-usage --mobile-disable
```

Quit and reopen Agents Usage after either command.

## Troubleshooting

**The phone cannot reach a LAN address**

- Confirm both devices are on the same Wi-Fi/LAN and client isolation or guest-network isolation is disabled.
- Confirm Agents Usage was restarted after mobile access was enabled.
- Allow inbound TCP `3765` in the desktop firewall for the private network only.
- Check that the desktop address has not changed. A DHCP reservation can keep it stable.

**The Tailscale address does not load**

- Confirm both devices appear connected in `tailscale status`.
- Run `tailscale serve status` on the desktop and use its exact HTTPS hostname and `/agents-usage` path.
- Confirm the local desktop view responds before involving Tailscale: `http://127.0.0.1:3765`.

**The app says pairing was rejected**

- Generate a new pairing link from the currently running desktop configuration.
- If the token was rotated, restart the desktop app and pair each phone again.
- Copy the whole link; truncated tokens are rejected.

**The app was installed but Android will not update it**

- Install release APKs from this repository over other release APKs. Debug builds use a separate package ID and signature.
- If Android reports a signature mismatch, remove the APK obtained from the other source before installing the official GitHub release. Removing an app also removes its saved connections.

## Security model

- The server is off by default and uses a random 256-bit bearer token.
- Pairing sets an HttpOnly, `SameSite=Strict` cookie. API calls without it receive `401 Unauthorized`.
- The Android WebView has file access, content access, geolocation, third-party cookies, and mixed content disabled. Certificate errors are rejected.
- Navigation is limited to saved desktop origins. HTTPS is required except for explicitly private address ranges.
- Android cloud backup and device-to-device transfer are disabled for the companion's connection and cookie data.
- The only mutating API available to the phone is **Refresh**. Account settings, credentials, paths, and reset-credit consumption are not served.

Anyone holding a pairing link can authenticate while they can reach the desktop. Treat it like a password: share it only through a trusted channel, never paste it into an issue or chat room, and rotate it if it may have leaked.
