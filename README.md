# Agents Usage

Agents Usage is a lightweight tray app that shows remaining OpenAI/Codex usage across local accounts. It obtains rate limits through each account's local Codex App Server and never reads or copies authentication tokens.

## Features

- Discovers Codex homes by `auth.json` evidence, not directory names.
- Preserves account names, colors, order, enabled state, and last valid usage.
- Coalesces refreshes and limits concurrent Codex processes.
- Starts hidden and delays its first background refresh by 10 seconds.
- Provides compact dashboard and scrollable Settings views.
- Uses GNOME, KDE Plasma, Windows, and macOS tray integrations.

Only OpenAI through Codex currently exposes the safe unattended remaining-quota data the app needs. Other provider logos are assets for possible future adapters; the app does not scrape credentials or display historical spend as remaining quota.

## Install on Linux

Install a release package from GitHub, or build and install for the current user:

```bash
./tools/build-release.sh
./tools/install-user.sh --autostart
```

The installer never logs out, reloads the desktop shell, terminates the app, or enables/disables extensions. A newly installed GNOME extension may become available only after you next log in; enable it manually when convenient.

Run `./tools/uninstall-user.sh` to remove the app. User settings remain under `~/.config/agents-usage/`.

## Build and verify

Requirements are Rust 1.92 and a working `codex` executable. Linux package builds additionally require rootless Podman, RPM, Flatpak, AppStream, and desktop-file tools.

```bash
cargo test --locked
cargo clippy --locked -- -D warnings
./tools/selftest.py
./tools/package-linux.sh
```

Linux output includes RPM, DEB, Flatpak, and AppImage files in `dist/packages/`. GitHub Actions builds an unsigned Windows x86-64 ZIP and an ad-hoc-signed universal macOS ZIP and DMG. Production signing and notarization are not configured.

## Discovery and configuration

Candidates come from `CODEX_HOME`, `AGENTS_USAGE_CODEX_HOMES`, saved paths, configured paths, and direct children of the user home containing `auth.json`. Every new candidate must return valid data through Codex before it is saved.

Configuration locations:

```text
Linux:   $XDG_CONFIG_HOME/agents-usage/config.toml
macOS:   ~/Library/Application Support/Agents Usage/config.toml
Windows: %APPDATA%\Agents Usage\config.toml
```

## Desktop support

- GNOME uses the bundled Shell extension for exact tray geometry, including Dash-to-Panel.
- KDE Plasma uses StatusNotifierItem.
- Windows and macOS use native tray APIs.
- Popup placement handles top, bottom, left, and right panels and clamps to the active monitor.

Linux packages are locally verified. Windows and macOS are compiled and packaged on native GitHub runners but remain unsigned and should be tested on their target systems.

## License

Agents Usage is licensed under [GPL-3.0-only](LICENSE). Third-party visual assets and their licenses are listed in [THIRD_PARTY_ASSETS.md](THIRD_PARTY_ASSETS.md).
