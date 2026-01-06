# hashtree-ci

Decentralized CI system built on Nostr and hashtree. Executes GitHub Actions workflows with cryptographically signed results stored in a content-addressed path space.

## Features

- **GitHub Actions compatible** - Parses `.github/workflows/*.yml` files
- **Decentralized** - No central server, results stored at runner's npub path
- **Cryptographically signed** - Results signed with runner's Nostr key (Schnorr)
- **Verifiable** - Anyone can verify results using runner's public key
- **Tag-based routing** - Jobs routed to runners based on `runs-on` tags

## Architecture

```
Repository                    Runner's Result Space
npub1owner/                   npub1runner/
├── repos/myproject/          └── ci/
│   ├── .hashtree/ci.toml        └── npub1owner/
│   ├── .github/workflows/           └── repos/myproject/
│   │   └── ci.yml                       └── <commit-sha>/
│   └── src/                                 ├── result.json
                                             └── logs/
```

Results are queryable by: `npub1runner/ci/<repo-owner-npub>/<repo-path>/<commit>/result.json`

## Installation

```bash
cargo install --path crates/ci-runner
```

## Quick Start

### 1. Initialize Runner Identity

```bash
# Basic (no container isolation - only for trusted repos)
htci init --name my-runner --tags linux,x64

# Recommended: with container isolation
htci init --name my-runner --tags linux,x64 --container
```

This creates `~/.config/hashtree-ci/runner.toml` with a new Nostr keypair:

```toml
[runner]
name = "my-runner"
nsec = "nsec1..."  # Keep secret!
tags = ["linux", "x64"]

[runner.limits]
max_concurrent_jobs = 4
job_timeout_secs = 3600

[runner.container]
enabled = true
runtime = "docker"
default_image = "ubuntu:22.04"
```

### 2. Configure Repository

Add `.hashtree/ci.toml` to your repository:

```toml
[ci]
[[ci.runners]]
npub = "npub1..."  # Your runner's public key
name = "linux-runner"
tags = ["linux"]
```

### 3. Create Workflow

Add `.github/workflows/ci.yml`:

```yaml
name: CI
on: [push, pull_request]

jobs:
  build:
    runs-on: linux
    steps:
      - name: Build
        run: cargo build --release

      - name: Test
        run: cargo test
```

### 4. Run CI

```bash
htci run /path/to/repo
```

## CLI Commands

```bash
htci init              # Generate new runner identity
htci whoami            # Show runner npub and tags
htci run <repo-path>   # Execute workflows for repository
htci results <repo>    # Query stored results
```

## Workflow Support

### Supported GitHub Actions Features

| Feature | Status |
|---------|--------|
| `on:` triggers (push, pull_request, etc.) | ✅ |
| `jobs:` with multiple jobs | ✅ |
| `runs-on:` (string or array) | ✅ |
| `needs:` (job dependencies) | ✅ |
| `if:` conditionals | ✅ |
| `env:` (job and step level) | ✅ |
| `run:` shell commands | ✅ |
| `working-directory:` | ✅ |
| `continue-on-error:` | ✅ |
| `timeout-minutes:` | ✅ |
| `uses:` actions | ⏳ Parsed, not executed |
| Container/Docker | ❌ Not yet |

### Example Workflow

```yaml
name: Build and Test

on:
  push:
    branches: [main]
  pull_request:

env:
  CARGO_TERM_COLOR: always

jobs:
  build:
    runs-on: [linux, x64]
    steps:
      - name: Check formatting
        run: cargo fmt --check

      - name: Clippy
        run: cargo clippy -- -D warnings

      - name: Build
        run: cargo build --release

      - name: Test
        run: cargo test
        continue-on-error: false

  deploy:
    needs: build
    runs-on: linux
    if: github.ref == 'refs/heads/main'
    steps:
      - name: Deploy
        run: ./deploy.sh
```

## Configuration

### Shared Network Config (`~/.hashtree/config.toml`)

Shared with hashtree-ts for Nostr relays and Blossom servers:

```toml
[network]
relays = [
  "wss://relay.damus.io",
  "wss://nos.lol",
  "wss://relay.nostr.band"
]

blossom_servers = [
  "https://blossom.primal.net",
  "https://cdn.satellite.earth"
]
```

### Runner Config (`~/.config/hashtree-ci/runner.toml`)

```toml
[runner]
name = "my-linux-runner"
nsec = "nsec1..."  # Private key - never share!
tags = ["linux", "x64", "docker", "gpu"]

[runner.limits]
max_concurrent_jobs = 4
job_timeout_secs = 3600
max_artifact_size = 1073741824  # 1GB

# Container isolation (recommended for security)
[runner.container]
enabled = true
runtime = "docker"  # or "podman"
default_image = "ubuntu:22.04"
network = "none"    # "none", "host", or "bridge"
memory_limit = "2g"
cpu_limit = "2"
rootless = true

# Restrict which repos can run on this runner
[[runner.allowed_repos]]
owner_npub = "npub1myorg..."
repo_pattern = "repos/*"
```

### Repository Config (`.hashtree/ci.toml`)

```toml
[ci]
# Optional: org that can dynamically authorize runners
org_npub = "npub1org..."

# Trusted runners (jobs only accepted from these)
[[ci.runners]]
npub = "npub1runner1..."
name = "linux-x64"
tags = ["linux", "x64", "docker"]

[[ci.runners]]
npub = "npub1runner2..."
name = "macos-arm64"
tags = ["macos", "arm64"]
```

## Result Format

Results are stored as signed JSON:

```json
{
  "job_id": "550e8400-e29b-41d4-a716-446655440000",
  "runner_npub": "npub1runner...",
  "repo_hash": "npub1owner/repos/myproject",
  "commit": "abc123def456",
  "workflow": ".github/workflows/ci.yml",
  "job_name": "build",
  "status": "success",
  "started_at": "2025-01-06T10:00:00Z",
  "finished_at": "2025-01-06T10:05:30Z",
  "logs_hash": "sha256:...",
  "steps": [
    {
      "name": "Build",
      "status": "success",
      "exit_code": 0,
      "duration_secs": 300,
      "logs_hash": "sha256:..."
    }
  ],
  "signature": "<schnorr-signature>"
}
```

## Integration with hashtree-ts

hashtree-ci integrates with [hashtree-ts](https://github.com/mmalmi/hashtree-ts) for displaying CI status in the web UI:

1. **Shared identity** - Both use Nostr npub/nsec keypairs
2. **Shared config** - Network settings in `~/.hashtree/config.toml`
3. **Path-based lookup** - UI queries `npub1runner/ci/<repo>/<commit>/result.json`
4. **WebRTC sync** - Results sync between peers via WebRTC
5. **Trust model** - Repos define trusted runners in `.hashtree/ci.toml`

### Lookup Flow

```
1. UI loads repo at npub1owner/repos/myproject
2. Reads .hashtree/ci.toml → gets trusted runner npubs
3. For current commit, queries each runner:
   npub1runner/ci/npub1owner/repos/myproject/<commit>/result.json
4. Displays status badge (✓ success / ✗ failure / ⏳ pending)
```

## Security

### Container Isolation

**Strongly recommended**: Enable container isolation to prevent CI jobs from accessing your host system.

```bash
# Initialize with container support
htci init --name my-runner --tags linux --container
```

When container mode is enabled:
- Jobs run inside Docker/Podman containers
- Network is disabled by default (`--network=none`)
- Filesystem is read-only except `/workspace` and `/tmp`
- Runs as non-root user (uid 1000)
- All capabilities dropped (`--cap-drop=ALL`)
- No new privileges (`--security-opt=no-new-privileges`)

### Allowed Repos

Restrict which repositories can run on your runner:

```toml
# Only accept jobs from specific npub
[[runner.allowed_repos]]
owner_npub = "npub1abc..."
repo_pattern = "repos/*"

# Accept jobs from any repo under your own npub
[[runner.allowed_repos]]
owner_npub = "npub1your..."
repo_pattern = "*"
```

### Trust Model

1. **Manual runs** - `htci run` always works for local repos
2. **Remote jobs** - Only accepted from repos in `allowed_repos` config
3. **Result verification** - All results cryptographically signed with runner's key
4. **Repo trust** - Repos define trusted runners in `.hashtree/ci.toml`

## Crates

| Crate | Description |
|-------|-------------|
| `ci-core` | Core types: Job, JobResult, Runner, Config |
| `ci-workflow` | GitHub Actions YAML parser |
| `ci-runner` | CLI binary (`htci`) and job executor |
| `ci-store` | Storage backends (filesystem, memory) |

## Development

```bash
# Build all crates
cargo build

# Run tests
cargo test

# Run CI runner
cargo run -p ci-runner -- run /path/to/repo
```

## License

MIT
