# five-nine

A terminal UI for monitoring Claude, OpenAI, and Apple Developer service status in real time.

![Rust](https://img.shields.io/badge/rust-2024-orange)
![Version](https://img.shields.io/badge/version-0.2.3-blue)

## Features

- Live status for Claude, OpenAI, and Apple Developer service components
- Add any service that uses Atlassian Statuspage with `five-nine add <name>`
- 90-day uptime percentage per service
- Airport-style animation (departing planes + ATC tower) colored by Claude Code's current status
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

### Custom providers

Add any service that uses Atlassian Statuspage — five-nine auto-discovers the API:

```bash
five-nine add github          # auto-discover
five-nine add stripe --url https://www.stripestatus.com   # explicit URL
five-nine list                # show all providers
five-nine remove github       # remove a custom provider
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
