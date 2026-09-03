<div align="center">
  <img src="https://raw.githubusercontent.com/tappunk/.github/refs/heads/main/assets/tnk.webp" alt="tnk" width="280"/>

# tnk (experimental)

**Zero-trust per-project sandbox VMs for AI agent runtimes.**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Crates.io](https://img.shields.io/crates/v/tnk?color=orange)](https://crates.io/crates/tnk)
[![GitHub Release](https://img.shields.io/github/v/release/tappunk/tnk/releases)](https://github.com/tappunk/tnk/releases)
[![X Follow](https://img.shields.io/twitter/follow/tappunk?style=social)](https://x.com/tappunk)

[Quick Start](#quick-start) · [Full Docs](https://tappunk.com/tnk/)
</div>

---

## Quick Start

![tnk demo](https://raw.githubusercontent.com/tappunk/.github/refs/heads/main/assets/_sandbox-oc-pi.gif)

```bash
brew tap tappunk/tap              # or: cargo install tnk
brew trust tappunk/tap            # required on recent Homebrew versions
brew install tappunk/tap/tnk
tnk init                          # populate ~/.config/tnk from tnk-specs
tnk config init                   # create ~/.config/tnk/tnk.toml
```

Point `default_model` at the model your host inference server serves:

```toml
# ~/.config/tnk/tnk.toml
default_model = "ai-fast"
```

Then start and enter a project sandbox:

```bash
cd ~/code/myproject
tnk sandbox start                 # boots VM, provisions the default profile
tnk sandbox shell
```

The agent runs in an isolated sandbox that mounts only the project workspace. Host secrets and keys stay out of scope. Inference runs on the host (tnk does not manage the engine); the sandbox gets endpoint and model coordinates via environment variables (`TNK_INFERENCE_URL`, `TNK_MODEL_NAME`, `TNK_ENGINE_RUNTIME`).

## What tnk does

- **Sandbox isolation**: one per-project Lima VM, mounting only the workspace directory
- **Provisioning**: declarative per-profile provisioning from `sandbox.d/provision.d`
- **Session audit trail**: optional NDJSON logs for forensic review
- **Machine-readable output**: `--output json|ndjson` on list commands

## Commands

```bash
tnk                # list sandboxes
tnk run            # start project sandbox
tnk sandbox shell  # enter project sandbox
tnk shutdown       # stop all sandboxes
tnk doctor         # health checks
tnk config show    # inspect effective config
```

## Config

Config lives at `~/.config/tnk/tnk.toml`. See the [full docs](https://tappunk.com/tnk/) for all settings and options.

## Security

Agents execute package installers, shell commands, and network clients with broad filesystem access. tnk keeps that execution inside isolated sandbox VMs, mounts only the project workspace, and exposes inference endpoints via explicit environment variables.

See [Security](https://tappunk.com/tnk/security) for the full threat model.

---

**Full documentation:** <https://tappunk.com/tnk/>
