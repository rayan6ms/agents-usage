# Provider support

Agents Usage discovers providers from the same local state their official command-line tools use. Install a CLI, sign in through that CLI, then press **Refresh** in Agents Usage. No provider token, account path, or server address normally needs to be entered.

## Capability matrix

| Provider | Prerequisite | Quota source | Supported result |
| --- | --- | --- | --- |
| [OpenAI Codex](https://developers.openai.com/codex/app-server/) | `codex` on `PATH`, signed in | Local Codex App Server | All returned rate-limit windows and reset credits |
| [OpenCode Go](https://dev.opencode.ai/docs/go/) | OpenCode with an active `opencode-go` connection | `opencode.ai/zen/go/v1/usage` | Rolling 5-hour, weekly, and monthly windows |
| [Anthropic Claude](https://code.claude.com/docs/en/statusline) | Claude Code subscription sign-in | Anthropic OAuth usage endpoint used by Claude Code | Session, weekly, and any model-scoped windows returned for the account |
| [Google Gemini](https://github.com/google-gemini/gemini-cli) | Gemini CLI Google OAuth sign-in | Gemini Code Assist `retrieveUserQuota` | Every model quota bucket and its reset time |
| [Cursor](https://docs.cursor.com/en/cli/reference/authentication) | Cursor Agent CLI on `PATH`, signed in | `cursor-agent status` | Account/authentication discovery only |
| [xAI Grok](https://x.ai/news/grok-build-cli) | Grok CLI sign-in | Local auth plus the CLI's zero-cost `grok models` check | Account discovery only |

[Cursor does not currently expose individual-plan usage through its CLI or a public API](https://forum.cursor.com/t/usage-api-cli-command/160967). [Grok's unified weekly consumer pool is shown in Grok Settings](https://docs.x.ai/grok/faq), not through Grok Build or a documented public API. Agents Usage shows those limitations directly instead of scraping cookies or presenting guessed values. If either provider adds a supported usage surface, its adapter can add bars without changing the desktop or phone data model.

## Discovery and refresh behavior

- Discovery runs on first launch and after every manual Refresh. Each provider is probed independently, so one unavailable service cannot block another.
- A successful response replaces only that provider account's snapshot. A timeout, rate limit, or temporary service failure keeps the last successful bars visible and adds a short status message.
- Strong evidence of a provider sign-in creates a visible row even when its first usage check fails, so expired authentication is actionable rather than silently ignored.
- Provider identity is part of every preference and cache key. Accounts from different providers cannot overwrite one another even if their local directories happen to match.
- Every returned quota bucket is retained. Monthly or weekly periods take the compact row when present; otherwise the longest known period, or the most constrained model bucket, is surfaced. Additional periods appear under the account chevron, and the existing short-period preference can still pin the first short window.

## Authentication and privacy

OpenAI Codex is queried through its local App Server, so Agents Usage never reads its token. OpenCode Go, Claude, and Gemini do not expose an equivalent local usage service; for those providers Agents Usage reads the existing CLI credential only in memory and sends it solely to that provider's official HTTPS quota endpoint. Credentials are never included in the usage cache, mobile API, logs, or phone companion.

The Gemini adapter refreshes an expired access token with the installed-application OAuth client published by Gemini CLI, without changing the CLI's credential file. Claude asks the installed Claude CLI to validate/refresh its sign-in before reading usage. On macOS it supports Claude Code's Keychain-backed credential as well as its configuration file.

## Troubleshooting

- **Provider is missing:** run its official CLI once and complete sign-in, then press Refresh. Confirm the executable is on the desktop app's `PATH` where required.
- **OpenCode appears but not OpenCode Go:** a normal OpenCode Zen key is not a Go subscription. Connect the `opencode-go` provider in OpenCode.
- **Claude says sign-in expired:** run `claude auth login`, then Refresh.
- **Gemini says sign-in expired or Code Assist is unavailable:** run `gemini`, select Google sign-in, and complete any project or eligibility prompt there.
- **Cursor is missing:** install Cursor Agent CLI and run `cursor-agent login`. A valid row will still explain that plan-usage bars are unavailable.
- **Grok is missing:** install Grok CLI and complete its login flow. A valid row will still explain that the weekly consumer pool has no supported API.
