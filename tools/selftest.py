#!/usr/bin/env python3
from pathlib import Path
import json
import re

root = Path(__file__).resolve().parents[1]
required = [
    'Cargo.toml', '.cargo/config.toml', 'build.rs', 'README.md', 'LICENSE', 'CHANGELOG.md',
    'CONTRIBUTING.md',
    'src/main.rs', 'src/codex.rs', 'src/config.rs', 'src/discovery.rs', 'src/providers.rs',
    'src/domain.rs', 'src/mobile.rs', 'src/ui_model.rs', 'ui/app.slint',
    'mobile/index.html', 'mobile/app.css', 'mobile/app.js',
    'mobile/manifest.webmanifest', 'mobile/sw.js',
    'mobile/icon-192.png', 'mobile/icon-512.png',
    'assets/icons/triangle-alert.svg',
    'assets/providers/openai.svg', 'assets/providers/opencode.svg',
    'assets/providers/anthropic.svg', 'assets/providers/gemini.svg',
    'assets/providers/cursor.svg', 'assets/providers/xai.svg', 'docs/providers.md',
    'integration/gnome-shell/extension/extension.js',
    'integration/gnome-shell/extension/metadata.json',
    'packaging/linux/agents-usage.desktop.in',
    'packaging/linux/agents-usage-autostart.desktop.in',
    'packaging/linux/agents-usage.svg',
    'packaging/linux/agents-usage.spec.in',
    'packaging/linux/debian-control.in',
    'packaging/linux/io.github.agentsusagetray.AgentsUsage.metainfo.xml',
    'packaging/linux/portable-launcher.sh',
    'packaging/macos/Info.plist.in',
    'tools/build-release.sh', 'tools/package-linux.sh', 'tools/verify-packages.sh',
    'tools/package-macos.sh', 'tools/package-windows.ps1',
    'tools/install-user.sh', 'tools/uninstall-user.sh',
    '.github/workflows/release.yml',
]
for rel in required:
    assert (root / rel).is_file(), rel

cargo = (root / 'Cargo.toml').read_text()
assert 'name = "agents-usage"' in cargo
assert 'license = "GPL-3.0-only"' in cargo
version = re.search(r'^version\s*=\s*"([^"]+)"', cargo, re.M).group(1)
assert '[profile.release]' in cargo
assert 'opt-level = "z"' in cargo
assert 'lto = "fat"' in cargo
assert 'strip = "symbols"' in cargo
linker_config = (root / '.cargo/config.toml').read_text()
assert '--icf=all' in linker_config
assert 'pack-relative-relocs' in linker_config

meta = json.loads((root / 'integration/gnome-shell/extension/metadata.json').read_text())
assert meta['uuid'] == 'agents-usage@local'

main = (root / 'src/main.rs').read_text()
codex = (root / 'src/codex.rs').read_text()
ui = (root / 'ui/app.slint').read_text()
ext = (root / 'integration/gnome-shell/extension/extension.js').read_text()
mobile_js = (root / 'mobile/app.js').read_text()
mobile_css = (root / 'mobile/app.css').read_text()
providers = (root / 'src/providers.rs').read_text()
android_build = (root / 'mobile-android/app/build.gradle').read_text()
android_main = (root / 'mobile-android/app/src/main/java/io/github/agentsusagetray/companion/MainActivity.java').read_text()
release_workflow = (root / '.github/workflows/release.yml').read_text()

assert 'account/rateLimits/read' in codex
assert 'READ_ATTEMPTS: usize = 3' in codex
assert 'account/rateLimitResetCredit/consume' in codex
assert 'experimentalApi": false' in codex
assert '"name": "agents-usage"' in codex
assert 'discover_new_accounts' in main and 'refresh_known_accounts' in main
assert 'RefreshIfStale' in main and 'OPEN_REFRESH_FRESHNESS' in main
assert 'let open_on_start = launch_mode(arguments)' in main
assert 'mobile::serve' in main and '--mobile-pairing-url' in main
assert 'activate_existing_instance_async(open_on_start)' in main
assert 'RefreshAtStartup' not in main
assert 'STARTUP_REFRESH_DELAY' not in main
assert 'INTERACTIVE_REFRESH_CONCURRENCY: usize = 8' in main
assert 'load_usage_cache' in main and 'save_usage_cache' in main
assert 'CheckPopupFocus' in main
assert 'XinputRawButtonPress' not in main
assert 'save_pending_reset' in main and 'idempotency_key' in main
assert 'revealAndHold(DTP_TEMPORARY_HOLD' in ext and 'release?.(DTP_TEMPORARY_HOLD)' in ext
assert 'for account[index] in root.accounts' in ui
assert 'account-name-changed' in ui and 'account-color-changed' in ui
assert 'account-custom-color-changed' in ui and 'HsvColorPicker' in ui
assert 'blur-names-changed' in ui and 'color-reset-timers-changed' in ui
assert 'root.provider-id == "openai": Path' in ui
assert 'assets/providers/cursor.svg' in ui
for provider in ['openai', 'opencode', 'anthropic', 'google', 'cursor', 'xai']:
    assert f'"{provider}"' in providers
    assert f'"{provider}" => PROVIDER_' in (root / 'src/mobile.rs').read_text()
assert 'zen/go/v1/usage' in providers
assert '/api/oauth/usage' in providers
assert 'retrieveUserQuota' in providers
assert 'cursor.com/api/usage-summary' in providers and 'WorkosCursorSessionToken' in providers
assert 'cli-chat-proxy.grok.com/v1/billing?format=credits' in providers
assert 'x-xai-token-auth' in providers and 'creditUsagePercent' in providers
assert 'gemini-credentials.json' in providers and 'secret_service::SecretService' in providers
assert 'UsageBarColorSettings' in ui and 'usage-bar-color-mode-changed' in ui
assert 'Green → red' in ui and 'usage-bar-custom-color-changed' in ui
assert 'account-move-requested' in ui
assert 'settings-height-px' in ui
assert 'vertical-scrollbar-policy: as-needed' in ui
assert 'if root.enabled-account-count > 0: DashboardScrollView' in ui
assert 'root.account.show-separator ? 1px : 0px' in ui
assert 'assets/icons/triangle-alert.svg' in ui and 'if root.account.has-error: AccountWarning' in ui
assert 'padding-bottom: 17px' not in ui
assert 'title: "Show banked resets"' in ui
assert 'padding-top: 8px' in ui
assert 'Personal' not in ui and 'team@anthropic.example' not in ui
assert 'StatusNotifierTray' in main and 'create_native_tray' in main
assert 'MoveFileExW' in (root / 'src/config.rs').read_text()
assert '<span class="reset-text"> • resets in <span class="reset-timer ' in mobile_js
assert '.reset-timer.colored' in mobile_css and '.reset-text.colored' not in mobile_css
assert '.account:last-child { border-bottom: 0; }' in mobile_css
assert 'class="warning-icon"' in mobile_js and './warning-icon.svg' in mobile_css
assert 'always_show_reset_counter' in mobile_js and 'pin_short_global' not in mobile_js
assert 'show_banked_resets' in mobile_js
assert 'STATE_CACHE_KEY' in mobile_js and 'restoreCachedState' in mobile_js
assert 'Always show reset counters' in ui and 'Always show 5-hour limits' not in ui
assert 'Paste & pair' in android_main and 'Back to usage' in android_main
assert 'LOAD_CACHE_ELSE_NETWORK' in android_main and 'usagePageAvailable' in android_main
assert 'dangerButton("Remove")' in android_main and 'R.drawable.ic_delete' in android_main
assert 'Color.rgb(39, 191, 206)' not in android_main
assert 'applicationId "io.github.agentsusagetray.companion"' in android_build
assert 'applicationIdSuffix ".debug"' in android_build
assert 'Agents-Usage-${version}-android.apk' in release_workflow
assert not (root / 'tools/backup-android-signing-key.sh').exists()

for rel in ['src/main.rs', 'src/codex.rs', 'ui/app.slint', 'integration/gnome-shell/extension/extension.js']:
    text = (root / rel).read_text()
    assert text.count('{') == text.count('}'), (rel, 'brace imbalance')

for rel in ['tools/build-release.sh', 'tools/package-linux.sh', 'tools/verify-packages.sh', 'tools/package-macos.sh', 'tools/install-user.sh', 'tools/uninstall-user.sh', 'tools/enable-autostart.sh', 'tools/disable-autostart.sh', 'packaging/linux/portable-launcher.sh']:
    assert (root / rel).stat().st_mode & 0o111, f'{rel} is not executable'

print(f'Agents Usage {version} static self-test: PASS')
