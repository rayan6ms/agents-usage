# Provider support

Agents Usage discovers providers from the same local state their official command-line tools use. Install a CLI, sign in through that CLI, then press **Refresh** in Agents Usage. No provider token, account path, or server address normally needs to be entered.

## Capability matrix

| Provider | Prerequisite | Quota source | Supported result |
| --- | --- | --- | --- |
| [OpenAI Codex](https://developers.openai.com/codex/app-server/) | `codex` on `PATH`, signed in | Local Codex App Server | All returned rate-limit windows and reset credits |
| [OpenCode Go](https://dev.opencode.ai/docs/go/) | OpenCode with an active `opencode-go` connection | `opencode.ai/zen/go/v1/usage` | Rolling 5-hour, weekly, and monthly windows |
| [Anthropic Claude](https://code.claude.com/docs/en/statusline) | Claude Code subscription sign-in | Anthropic OAuth usage endpoint used by Claude Code | Session, weekly, and any model-scoped windows returned for the account |
| [Google Gemini](https://github.com/google-gemini/gemini-cli) | Gemini CLI Google OAuth sign-in | Gemini Code Assist `retrieveUserQuota` | Every model quota bucket and its reset time |
| [Google Antigravity](https://antigravity.google/docs/cli/commands/usage) | Signed in to Google’s standalone Antigravity CLI (`agy`) or T3 Code | Official `agy -p /usage --output-format json` CLI, T3 Code’s local status cache, or stdin import | Reported quota windows and reset times |
| [Cursor](https://docs.cursor.com/en/cli/reference/authentication) | Cursor Agent CLI on `PATH`, signed in | Cursor's authenticated usage-summary endpoint | Included, Auto, and API usage; individual and team-plan fallbacks |
| [xAI Grok](https://x.ai/news/grok-build-cli) | Grok CLI sign-in | Grok CLI's authenticated billing endpoint | Included-credit usage and the returned weekly or monthly period |

Cursor and Grok do not currently document these usage endpoints as stable public APIs. Agents Usage follows the authenticated, first-party request formats used by current CLI/account integrations, validates the returned periods and percentages, and keeps the last successful snapshot when a request is unavailable or rate-limited. It never derives a bar from spend or invents a quota period.

## Discovery and refresh behavior

- Discovery runs on first launch and after every manual Refresh. Each provider is probed independently, so one unavailable service cannot block another.
- A successful response replaces only that provider account's snapshot. A timeout, rate limit, or temporary service failure keeps the last successful bars visible and adds a short status message.
- Strong evidence of a provider sign-in creates a visible row even when its first usage check fails, so expired authentication is actionable rather than silently ignored.
- Provider identity is part of every preference and cache key. Accounts from different providers cannot overwrite one another even if their local directories happen to match.
- Every returned quota bucket is retained. Monthly or weekly periods take the compact row when present; otherwise the longest known period, or the most constrained model bucket, is surfaced. Additional periods appear under the account chevron, and the existing short-period preference can still pin the first short window.

## Authentication and privacy

OpenAI Codex is queried through its local App Server, so Agents Usage never reads its token. The other providers do not expose an equivalent local usage service; for them Agents Usage reads the existing CLI credential only in memory and sends it solely to that provider's first-party HTTPS quota endpoint. Credentials are never included in the usage cache, mobile API, logs, or phone companion.

The Gemini adapter refreshes an expired access token with the installed-application OAuth client published by Gemini CLI, without changing the CLI's credential file. Claude asks the installed Claude CLI to validate/refresh its sign-in before reading usage. On macOS it supports Claude Code's Keychain-backed credential as well as its configuration file. Grok delegates expired-session refresh to its installed CLI and then rereads the credential; Cursor reports an expired sign-in without modifying it.

## Google Antigravity quota

Agents Usage keeps Antigravity quota up to date without reading Google OAuth tokens or calling Google quota endpoints directly:

1. **T3 Code Antigravity accounts:** Agents Usage automatically discovers every personal Google Antigravity sign-in configured in T3 Code (`~/.t3/userdata/providers/antigravity/*`), tracks each account separately, and refreshes each profile through the official Antigravity CLI in an isolated temporary environment.
2. **Official standalone Antigravity CLI (`agy`):** When Google’s standalone `agy` CLI is installed and signed in (and no T3 Code profiles cover it), opening the tray or pressing **Refresh** runs `agy -p /usage --output-format json` non-interactively (never launching a browser login flow) and updates the dashboard and phone companion with the reported windows and reset times.
3. **T3 Code local cache:** When T3 Code’s non-secret provider status cache (`~/.t3/caches/*.json`) holds fresher `usageLimits`, Agents Usage can also adopt those readings.
4. **Manual or status-line import:** You can still pipe a report directly at any time:

```sh
agy -p /usage --output-format json | agents-usage --import-antigravity-usage
```

In Agents Usage, **Settings → Antigravity quota → Copy import command** copies this command. If you use the standalone Antigravity CLI interactively, its [documented status line](https://antigravity.google/docs/cli/statusline/) can also send quota to `agents-usage --import-antigravity-usage --quiet`. Whichever source has the newest timestamp wins, while preserving known account identity and plan metadata. A failed refresh keeps the previous reading visible.

## Troubleshooting

- **Provider is missing:** run its official CLI once and complete sign-in, then press Refresh. Confirm the executable is on the desktop app's `PATH` where required.
- **OpenCode appears but not OpenCode Go:** a normal OpenCode Zen key is not a Go subscription. Connect the `opencode-go` provider in OpenCode.
- **Claude says sign-in expired:** run `claude auth login`, then Refresh.
- **Gemini says sign-in expired or Code Assist is unavailable:** run `gemini`, select Google sign-in, and complete any project or eligibility prompt there.
- **Cursor is missing or expired:** install Cursor Agent CLI and run `cursor-agent login`, then Refresh.
- **Grok is missing or expired:** install Grok CLI and complete its login flow. Agents Usage asks the CLI to refresh an expired session once before reporting that login is required.
