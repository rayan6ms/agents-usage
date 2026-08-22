# Phone companion setup

The phone companion is a read-only view of usage already collected by the desktop app. Codex stays installed and signed in on the desktop. The phone cannot view credentials or local paths, change account settings, or consume reset credits; its only desktop action is Refresh.

## Requirements and supported configurations

- Agents Usage 0.3.0 or newer, running in a logged-in desktop session.
- Android 8.0/API 26 or newer with Android System WebView enabled.
- For LAN: both devices on the same private network without guest/client isolation.
- For remote access: Tailscale on both devices, signed into the same tailnet.

Linux, Windows, and macOS builds contain the same companion server. Linux is exercised end-to-end on physical hardware; Windows and macOS are compiled and tested on native CI runners and have platform-specific guidance below. Android CI runs the companion on API 26 and a current API emulator. The browser/PWA view may work on iPhone and iPad over Tailscale HTTPS, but iOS remains an experimental, non-native support tier.

The desktop must remain running and awake. Root, ADB, Termux, a USB cable, a cloud account, a public server, port forwarding, and Tailscale Funnel are not required or recommended.

## 1. Install and update Android

1. Open the [latest release](https://github.com/rayan6ms/agents-usage/releases/latest) on the phone.
2. Download `Agents-Usage-VERSION-android.apk` and its `.sha256` file.
3. Optionally verify the APK checksum, then open it. Android may request permission for the browser or file manager to install unknown apps.
4. Future release APKs install over the existing application and retain its connections. **Check for app updates** in the Connections screen opens the official releases page. Obtainium users can also track this repository's APK releases.

The official signing-certificate SHA-256 fingerprint is:

```text
c9a8159f30df08f5b5613ea0b438d4746c292f600aca3bc9ab48f5c5d7a540bf
```

Debug builds have a different package ID and signing key and can coexist with the release app.

## 2. Pair from desktop settings

1. Open Agents Usage **Settings**.
2. Turn on **Phone companion**. The server starts immediately on loopback TCP `3765`; no restart is needed.
3. Choose the routes you actually need:
   - For private remote access, press **Set up Tailscale**. This runs the equivalent of:

     ```bash
     tailscale serve --bg --set-path /agents-usage http://127.0.0.1:3765
     ```

   - For direct same-network access, turn on **Allow direct LAN**. This changes the listener to all local interfaces; only do this on a network you trust.

4. Press **Pair a phone**. The desktop detects its primary private LAN route and Tailscale DNS name and creates one QR code containing the available addresses.
5. Scan the QR code with the phone's normal camera. If its camera does not open custom links, use **Copy private link**, transfer it through a trusted channel, then paste it into the Android app.

The link expires after ten minutes. The phone pairs each included origin, saves only the non-secret base addresses, tests them through an authenticated health endpoint, and opens the first working one.

### What still requires manual approval

- Android must approve installation when the APK is installed outside an app store.
- If **Allow direct LAN** is on, Windows, macOS, or a Linux firewall may ask whether TCP `3765` is allowed on private networks.
- Tailscale must be installed, signed in, and permitted by its tailnet policy. Windows may require an elevated terminal to configure Serve.
- A guest Wi-Fi network may intentionally prevent devices from reaching one another; switch networks or use Tailscale.

These prompts protect system or network boundaries and are intentionally not bypassed.

## Connections and failover

The Android app health-checks the current endpoint every 15 seconds and whenever Android reports a network change. It does not rely on a cached web page to decide whether the desktop is reachable. If LAN fails, it probes Tailscale; when LAN returns it continues using the current healthy route until a reconnect is needed.

Tap the visible Connections icon in the usage header, or press Android Back, to manage addresses:

- **Use** probes and selects an address.
- **Remove** deletes the saved address and its cookie from the phone.
- **Check for app updates** opens only the official GitHub releases page.

Removing an address locally does not revoke other sessions for that phone. To invalidate the phone everywhere, use **Revoke** beside it in desktop settings. LAN and Tailscale cookies issued during one QR pairing are grouped as one phone.

## Security and privacy

- The server is disabled by default. New configurations bind to loopback after it is enabled; listening on the LAN requires the separate **Allow direct LAN** choice. Existing configurations that already used a LAN bind keep working after upgrade.
- Pairing uses a random 256-bit token, stored as a SHA-256 hash on the desktop. It expires after ten minutes and has only enough redemptions for the addresses in that pairing bundle.
- Each phone receives independent random sessions. Only session hashes are stored. Sessions expire after 180 days and can be revoked individually.
- Cookies are HttpOnly, `SameSite=Strict`, and scoped to `/agents-usage/` behind the Tailscale path rather than the rest of that hostname. HTTPS cookies are marked Secure.
- The server uses constant-time hash comparison and rate-limits forced Refresh requests.
- Android rejects certificate errors and public cleartext hosts. File/content access, geolocation, mixed content, third-party cookies, cloud backup, and device-to-device backup are disabled.
- The WebView can navigate only inside paired origins. Opening the official releases page is a separate, explicit Android action.
- Do not use router port forwarding or Tailscale Funnel. Anyone holding an unexpired pairing link and able to reach the desktop can pair, so treat the link like a temporary password.

## Platform notes

### Windows

If **Allow direct LAN** is enabled, allow `agents-usage.exe` on **Private networks** if Windows Defender Firewall prompts. Tailscale Serve normally requires an Administrator PowerShell or Terminal. If the desktop user logs out, the tray app stops even when Tailscale unattended mode is enabled.

### macOS

If **Allow direct LAN** is enabled, allow incoming connections for Agents Usage if macOS asks. The tray app and companion stop being available when the user logs out or the Mac sleeps. Tailscale's system extension/App Store variants can expose the same Serve path while the user session is active.

### Linux

If **Allow direct LAN** is enabled and a firewall is active, allow TCP `3765` only from the trusted LAN subnet. Examples vary by distribution (`ufw`, `firewalld`, or nftables), so Agents Usage does not silently change firewall rules. Tailscale access does not require exposing `3765` beyond loopback.

### Address changes and IPv6

The pairing wizard selects the primary private route. On desktops with several VLANs, VPNs, or Wi-Fi adapters, use the command-line fallback below with the address reachable from the phone. A DHCP reservation or stable `.local` name avoids repeated LAN pairing after address changes. IPv4 and IPv6 bind addresses are supported; bracket IPv6 literals in URLs, for example `http://[fd00::20]:3765`.

## Command-line recovery and automation

The settings wizard is preferred. These commands remain available for headless recovery and scripted deployment:

```bash
agents-usage --mobile-enable
agents-usage --mobile-pairing-url http://192.168.1.20:3765
agents-usage --mobile-pairing-url https://DESKTOP.TAILNET.ts.net/agents-usage
agents-usage --mobile-rotate-token
agents-usage --mobile-disable
```

`--mobile-pairing-url` creates a new single-use, ten-minute link and an already-running server can import it. Because a separate command cannot safely reconfigure every running desktop process, CLI enable, disable, and revoke-all operations take full effect after Agents Usage is reopened; the settings controls apply immediately. The historical `--mobile-rotate-token` name now revokes every phone and pending pairing rather than maintaining a shared master token.

`--mobile-enable` is an explicit recovery/automation command and enables direct LAN listening. Use the desktop switch when you want the safer loopback-only Tailscale configuration.

## Troubleshooting

**No LAN address is shown**

- Turn on **Allow direct LAN** in desktop settings.
- Confirm the desktop has a private IPv4/IPv6 route and is not connected only through a captive portal.
- Multi-interface desktops can use `--mobile-pairing-url` with the correct reachable address.
- Confirm the firewall permits private-network TCP `3765`.

**Tailscale is not detected or setup fails**

- Run `tailscale status` and confirm the desktop and phone are in the same tailnet.
- On Windows, retry the displayed command in an elevated terminal.
- Confirm tailnet access-control rules permit the phone to reach the desktop.
- Run `tailscale serve status` and verify `/agents-usage` points to `http://127.0.0.1:3765`.

**Pairing was rejected**

- Generate a new QR/link; old links expire after ten minutes and cannot be reused beyond their included routes.
- Copy the complete link through a trusted channel.

**The view worked but became unavailable**

- Wake the desktop and confirm Agents Usage is still running.
- Open Connections; the app will probe every saved route rather than trusting its cached page.
- Check whether the desktop's DHCP address changed or Tailscale disconnected.

**Android refuses an update**

- Install release APKs from this repository over other official release APKs.
- A signature mismatch means the installed APK came from another signing key. Uninstalling it removes its saved connections, after which the official package can be installed and paired again.
