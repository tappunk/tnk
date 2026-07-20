<div align="center">
  <img src="https://raw.githubusercontent.com/tappunk/.github/refs/heads/main/assets/tnk.webp" alt="tnk" width="280"/>

# tnk (experimental)

**Zero-trust sandbox for local inference and secure AI coding agent runtimes.**

[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)
[![Crates.io](https://img.shields.io/crates/v/tnk?color=orange)](https://crates.io/crates/tnk)
[![GitHub Release](https://img.shields.io/github/v/release/tappunk/tnk)](https://github.com/tappunk/tnk/releases)
[![X Follow](https://img.shields.io/twitter/follow/tappunk?style=social)](https://x.com/tappunk)

[Quick Start](#quick-start) · [Full Docs](https://tappunk.com/tnk/)
</div>

---

## Quick Start

![tnk demo](https://raw.githubusercontent.com/tappunk/.github/refs/heads/main/assets/_sandbox-oc-pi.gif)

```bash
brew install tappunk/tnk/tnk      # or: cargo install tnk
tnk init                          # populate config from tnk-specs
tnk config init                   # create ~/.config/tnk/tnk.toml
tnk run                           # boot engine + services
```

Then enter a project sandbox:

```bash
cd ~/code/myproject
tnk sandbox start                 # auto installs default provision
tnk sandbox shell
```

The agent runs in an isolated sandbox that mounts only the project workspace. Host secrets and keys stay out of scope.

## What tnk does

- **Inference management**: start, stop, and query the local inference engine
- **Sandbox isolation**: one per-project sandbox, mounting only the workspace directory
- **Persistent services**: MCP bridge and search tooling, managed alongside the engine
- **Session audit trail**: optional NDJSON logs for forensic review
- **Machine-readable output**: `--output json|ndjson` on status commands

## Commands

```bash
tnk run              # boot engine + services
tnk sandbox shell    # enter project sandbox
tnk shutdown --yes   # tear down everything
tnk doctor           # health checks
tnk config show      # inspect effective config
```

## Config

Config lives at `~/.config/tnk/tnk.toml`. See the [full docs](https://tappunk.com/tnk/) for all settings and options.

## Security

Agents execute package installers, shell commands, and network clients with broad filesystem access. tnk keeps that execution inside isolated sandboxes, mounts only the project workspace, and exposes inference endpoints via explicit environment variables.

See [Security](https://tappunk.com/tnk/security) for the full threat model.

---

**Full documentation:** <https://tappunk.com/tnk/>
