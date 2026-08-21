#!/usr/bin/env python3
from pathlib import Path
import json
import re

root = Path(__file__).resolve().parents[1]
required = [
    'Cargo.toml', '.cargo/config.toml', 'build.rs', 'README.md', 'LICENSE', 'CHANGELOG.md',
    'src/main.rs', 'src/codex.rs', 'src/config.rs', 'src/discovery.rs',
    'src/domain.rs', 'src/mobile.rs', 'src/ui_model.rs', 'ui/app.slint',
    'mobile/index.html', 'mobile/app.css', 'mobile/app.js',
    'mobile/manifest.webmanifest', 'mobile/sw.js',
    'mobile/icon-192.png', 'mobile/icon-512.png',
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
assert 'UsageBarColorSettings' in ui and 'usage-bar-color-mode-changed' in ui
assert 'Green → red' in ui and 'usage-bar-custom-color-changed' in ui
assert 'account-move-requested' in ui
assert 'settings-height-px' in ui
assert 'vertical-scrollbar-policy: as-needed' in ui
assert 'if root.enabled-account-count > 0: ScrollView' in ui
assert 'padding-top: 8px' in ui
assert 'Personal' not in ui and 'team@anthropic.example' not in ui
assert 'StatusNotifierTray' in main and 'create_native_tray' in main
assert 'MoveFileExW' in (root / 'src/config.rs').read_text()

for rel in ['src/main.rs', 'src/codex.rs', 'ui/app.slint', 'integration/gnome-shell/extension/extension.js']:
    text = (root / rel).read_text()
    assert text.count('{') == text.count('}'), (rel, 'brace imbalance')

for rel in ['tools/build-release.sh', 'tools/package-linux.sh', 'tools/verify-packages.sh', 'tools/package-macos.sh', 'tools/install-user.sh', 'tools/uninstall-user.sh', 'tools/enable-autostart.sh', 'tools/disable-autostart.sh', 'packaging/linux/portable-launcher.sh']:
    assert (root / rel).stat().st_mode & 0o111, f'{rel} is not executable'

print(f'Agents Usage {version} static self-test: PASS')
