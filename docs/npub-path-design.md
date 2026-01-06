# CI npub/path Directory Structure Design

## Overview

All CI data lives in hashtree, queryable by npub/path. No separate API.

## Directory Structure

### Repo (owner's space)
```
npub1owner.../
└── repos/
    └── myproject/
        ├── .hashtree/
        │   └── ci.toml          # Lists trusted runner npubs
        ├── .github/
        │   └── workflows/
        │       └── ci.yml
        └── src/
```

### CI Results (runner's space)
```
npub1runner.../
└── ci/
    └── npub1owner/
        └── repos/
            └── myproject/
                └── <commit-sha>/
                    ├── result.json
                    └── logs/
                        ├── build.txt
                        └── test.txt
```

Repo identity = `npub/path` (e.g., `npub1owner.../repos/myproject`)

Results stored per-commit only. No "latest" or "status.json" cache.

## File Formats

### `.hashtree/ci.toml` (in repo)
```toml
[ci]
[[ci.runners]]
npub = "npub1runner1..."
name = "linux-x64"
tags = ["linux", "docker"]

[[ci.runners]]
npub = "npub1runner2..."
name = "macos-arm64"
tags = ["macos"]
```

### `result.json`
```json
{
  "runner_npub": "npub1runner...",
  "repo_npub": "npub1owner...",
  "repo_path": "repos/myproject",
  "commit": "abc123...",
  "workflow": ".github/workflows/ci.yml",
  "status": "success",
  "started_at": "2024-01-06T10:00:00Z",
  "finished_at": "2024-01-06T10:02:00Z",
  "steps": [
    {"name": "checkout", "status": "success", "duration_secs": 2},
    {"name": "build", "status": "success", "duration_secs": 45},
    {"name": "test", "status": "success", "duration_secs": 30}
  ],
  "signature": "schnorr_sig..."
}
```

## Lookup Flow

### UI shows CI status for repo at `npub1owner.../repos/myproject`:

```
1. Get repo HEAD commit (e.g., abc123)

2. Read repo's .hashtree/ci.toml
   → Get trusted runner npubs

3. For each runner, check:
   npub1runner.../ci/npub1owner/repos/myproject/abc123/result.json

4. If exists → show status (✓ passing / ✗ failing)
   If missing → show "CI pending" or "no CI for this commit"
```

No stale status. UI compares HEAD vs available CI commits directly.

### Runner watches repo for updates:

```
1. Subscribe via nostr resolver to:
   npub1owner.../repos/myproject

2. On root hash change:
   - Fetch new commit
   - Read .github/workflows/
   - Execute CI
   - Write result to:
     npub1runner.../ci/npub1owner/repos/myproject/<commit>/
```

## Benefits

- **No cache staleness** - Only commit-based results
- **Verifiable** - Results signed by runner, path proves provenance
- **Decentralized** - No API server, just hashtree lookups
- **Simple** - Path mirrors repo identity
