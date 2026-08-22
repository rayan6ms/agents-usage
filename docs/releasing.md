# Release checklist

## Android signing continuity

Android accepts an APK update only when it is signed by the same key as the installed app. Before publishing the first stable APK:

1. Store the release keystore outside the repository.
2. Create at least one encrypted backup on offline media:

   ```bash
   ./tools/backup-android-signing-key.sh /media/OFFLINE/agents-usage-android-key.jks.enc
   ```

3. Keep the encryption password in a separate password manager.
4. Test restoring it to a temporary directory with `openssl enc -d -aes-256-cbc -pbkdf2` and inspect it with `keytool -list`.
5. Confirm the certificate SHA-256 fingerprint is:

   ```text
   c9a8159f30df08f5b5613ea0b438d4746c292f600aca3bc9ab48f5c5d7a540bf
   ```

The GitHub release workflow independently verifies that fingerprint after signing.

## Preflight

1. Run `cargo test --locked`, `cargo clippy --locked -- -D warnings`, and `./tools/selftest.py`.
2. Run Android unit tests, lint, debug assembly, and the API 26/current managed-device jobs.
3. Dispatch **Release packages** with **Build and verify a signed Android preflight artifact** enabled. Download it, confirm its checksum, and install it over the previous release on a physical phone.
4. Test QR pairing over LAN and Tailscale, route failover, Refresh, relaunch, individual revoke, and APK upgrade without losing connections.
5. Tag only after every platform job and the signed preflight pass.

GitHub Actions are pinned to full commits and the Gradle wrapper verifies its distribution checksum. Dependabot or an equivalent reviewed change should update those pins deliberately.
