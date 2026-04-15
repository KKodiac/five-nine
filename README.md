# five-nine

Monitor any [Atlassian Statuspage](https://www.atlassian.com/software/statuspage) service from your terminal — live status, 90-day uptime, and an animated dashboard that reacts to the worst current incident across all your providers.

![Rust](https://img.shields.io/badge/rust-2024-orange)
![Version](https://img.shields.io/badge/version-0.2.6-blue)
![License](https://img.shields.io/badge/license-MIT-green)

## Features

- **Any Statuspage service** — Claude, OpenAI, GitHub, Vercel, Stripe, and thousands more
- **Auto-discovery** — `five-nine add github` finds the API automatically
- **90-day uptime** percentage per service component
- **Airport animation** — departing planes and ATC tower; color tracks worst active incident
- **Scrollable board** — works on any terminal height
- **Desktop notifications** — macOS alert when any provider degrades or recovers
- **Scriptable** — `five-nine status` and `five-nine status --json` for pipelines and alerting
- **Self-updating** — `five-nine update` downloads the latest binary in place
- Auto-refreshes every 30 seconds

## Install

```bash
brew tap KKodiac/five-nine
brew install five-nine
```

## Quick start

```bash
# Claude + OpenAI are added automatically on first run.
# Add more:
five-nine add github
five-nine add vercel

# Launch the TUI:
five-nine
```

## Commands

| Command | Description |
|---------|-------------|
| `five-nine` / `five-nine monitor` | Launch the TUI (default) |
| `five-nine status` | Print status table to stdout and exit |
| `five-nine status --json` | Machine-readable JSON output |
| `five-nine add <name>` | Auto-discover and add a provider |
| `five-nine add <name> --url <url>` | Add a provider with an explicit base URL |
| `five-nine remove <name>` | Remove a provider |
| `five-nine list` | List all configured providers |
| `five-nine update` | Self-update to the latest release |
| `five-nine uninstall` | Remove the binary and all configuration |
| `five-nine --version` | Print version |
| `five-nine --help` | Print help |

### TUI key bindings

| Key | Action |
|-----|--------|
| `r` | Force refresh |
| `↑` / `k` | Scroll up |
| `↓` / `j` | Scroll down |
| `PageUp` / `u` | Scroll up 10 lines |
| `PageDown` / `d` | Scroll down 10 lines |
| `q` / `Esc` | Quit |

### Provider config

Providers are stored in `~/.config/five-nine/providers.json`. Claude and OpenAI are seeded on first run; edit the file directly to reorder or customise entries.

## Supported services

Any service hosted on [Atlassian Statuspage](https://www.atlassian.com/software/statuspage) works automatically. Auto-discovery handles most of them — just use the service name:

```bash
five-nine add anthropic
five-nine add openai
five-nine add github
five-nine add gitlab
five-nine add stripe
five-nine add vercel
five-nine add netlify
five-nine add heroku
five-nine add pagerduty
five-nine add twilio
five-nine add sendgrid
five-nine add datadog
five-nine add cloudflare
five-nine add linear
five-nine add notion
five-nine add discord
five-nine add figma
five-nine add shopify
five-nine add intercom
five-nine add zendesk
five-nine add atlassian
five-nine add bitbucket
five-nine add npm
five-nine add rubygems
five-nine add docker
```

If auto-discovery doesn't find a service, pass the base URL explicitly:

```bash
five-nine add myservice --url https://status.myservice.com
```

Services with fully custom status pages (AWS Health, Azure Status, Apple Developer) are **not** supported — they don't expose the Statuspage v2 API.

## Build from source

```bash
cargo build --release
./target/release/five-nine
```

## Development

```bash
cargo test                   # unit tests (offline, fast)
cargo test -- --ignored      # integration tests (requires network)
```

## License

MIT
