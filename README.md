# five-nine

A terminal UI for monitoring any service that uses Atlassian Statuspage.

![Rust](https://img.shields.io/badge/rust-2024-orange)
![Version](https://img.shields.io/badge/version-0.2.4-blue)

## Features

- Monitor any Atlassian Statuspage service — Claude, OpenAI, GitHub, Vercel, Stripe, and more
- Auto-discovers status APIs by name; explicit URL override available
- 90-day uptime percentage per service
- Airport-style animation colored by the worst current status across all providers
- Scrollable service board
- Auto-refreshes every 30 seconds
- One-shot `status` command for scripting and shell pipelines

## Install

```bash
brew tap KKodiac/five-nine
brew install five-nine
```

## Usage

### TUI monitor (default)

```bash
five-nine
five-nine monitor   # explicit alias
```

| Key | Action |
|-----|--------|
| `r` | Force refresh |
| `↑` / `k` | Scroll up |
| `↓` / `j` | Scroll down |
| `PageUp` / `u` | Scroll up 10 lines |
| `PageDown` / `d` | Scroll down 10 lines |
| `q` / `Esc` | Quit |

### One-shot status check

```bash
five-nine status          # colored table to stdout
five-nine status --json   # machine-readable JSON
```

### Manage providers

All providers are user-configured. Claude and OpenAI are seeded on first run.

```bash
five-nine add github          # auto-discover status page
five-nine add stripe --url https://www.stripestatus.com   # explicit URL
five-nine list                # show all providers
five-nine remove github       # remove a provider
```

Config is saved to `~/.config/five-nine/providers.json`.

### Self-update

```bash
five-nine update
```

### Other

```bash
five-nine --version
five-nine --help
five-nine help <command>
```

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
