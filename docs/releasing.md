# Release checklist

## Android signing

Android requires every update to use the same signing certificate as the installed app. GitHub Actions handles signing automatically from repository secrets and verifies this certificate SHA-256 fingerprint before it publishes an APK:

   ```text
   c9a8159f30df08f5b5613ea0b438d4746c292f600aca3bc9ab48f5c5d7a540bf
   ```

There is no encrypted-key backup step in the project and no signing-key handling in the normal release flow. The keystore and passwords must not be committed to the repository.

## Release

1. Open a pull request and let CI run the desktop checks plus Android unit, lint, API 26, and current-API tests.
2. After CI succeeds on `main`, push a version tag matching `Cargo.toml` (for example, `v0.5.0`).
3. The tag workflow checks that the tag points to the current `main` commit, creates each platform package in parallel, verifies the Android signing certificate, and publishes the GitHub release.

The release workflow intentionally does not repeat the test and lint suite that CI has already completed for the same commit. If Android signing secrets or configuration changed, first dispatch **Release packages** with **Build and verify a signed Android preflight artifact** enabled; this exceptional preflight builds only the signed Android artifact rather than duplicating every desktop package.

GitHub Actions are pinned to full commits and the Gradle wrapper verifies its distribution checksum. Dependabot or an equivalent reviewed change should update those pins deliberately.
