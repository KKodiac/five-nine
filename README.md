# five-nine

A terminal UI for monitoring Claude service status in real time.

![Rust](https://img.shields.io/badge/rust-2024-orange)

## Features

- Live status for each Anthropic service component
- 90-day uptime percentage per service
- ECG-style animation colored by Claude Code's current status
- Auto-refreshes every 30 seconds

## Install

```bash
brew tap KKodiac/five-nine
brew install five-nine
```

## Usage

```bash
five-nine
```

| Key | Action |
|-----|--------|
| `r` | Force refresh |
| `q` / `Esc` | Quit |

## Build from source

```bash
cargo build --release
./target/release/five-nine
```

## License

MIT
