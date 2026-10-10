---
name: release
description: Cut a release of the onde-ed Rust workspace. Covers version bumping, the release branch/PR flow, tagging, and the two tag-triggered GitHub Actions (crates.io publish and Swift XCFramework). Use when asked to create, cut, or publish a release.
allowed-tools: Read, Write, Edit, Glob, Grep, Bash
user-invocable: true
---

# Skill: Release

## What This Skill Covers

How to cut a release of the `onde-ed` Cargo workspace: bumping the version,
landing it through the branch/PR flow, tagging, and what the two tag-triggered
workflows do automatically afterwards.

## The one-number rule

The whole workspace releases on **one version**. Every crate declares
`version.workspace = true`, so a release changes exactly one source of truth:
the root `Cargo.toml`.

```toml
[workspace.package]
version = "X.Y.Z"
```

Plus the internal path dependencies in the same file, which pin the version
explicitly:

```toml
ed-agent        = { version = "X.Y.Z", path = "crates/ed-agent" }
ed-mcp          = { version = "X.Y.Z", path = "crates/ed-mcp" }
ed-acp          = { version = "X.Y.Z", path = "crates/ed-acp" }
ed-acp-tui      = { version = "X.Y.Z", path = "crates/ed-acp-tui" }
ed-acp-testkit  = { version = "X.Y.Z", path = "crates/ed-acp-testkit" }
ed-onde-account = { version = "X.Y.Z", path = "crates/ed-onde-account" }
```

Bump all of these together. The crate manifests under `crates/*/Cargo.toml`
need no edits.

## Versioning

Semver. Given the current `X.Y.Z`:
- **patch** (`Z+1`) — bug fixes, internal changes, no API additions.
- **minor** (`Y+1`, `Z=0`) — backwards-compatible API additions.
- **major** (`X+1`, `Y=Z=0`) — breaking changes.

## Step-by-step

The base branch is **`development`**; `main` is the promoted/stable branch.
Releases land on `development` first, get tagged, then `development` is promoted
to `main`.

### 1. Branch off `development`

```sh
git checkout development && git pull
git checkout -b release/X.Y.Z
```

### 2. Bump the version

Edit the root `Cargo.toml` (`[workspace.package] version` and the internal
`ed-*` path-dependency versions). A quick way, run from the repo root:

```sh
sed -i '' 's/OLD\.VER\.SION/X.Y.Z/g' Cargo.toml
```

### 3. Refresh the lockfile and verify

```sh
cargo check --workspace   # updates Cargo.lock, catches breakage
```

`cargo fmt --all -- --check`, `cargo test --locked --workspace`, and
`cargo clippy --locked --workspace --all-targets -- -D warnings` must all pass —
the release workflow re-runs them and fails the publish otherwise.

### 4. Commit and push

```sh
git add Cargo.toml Cargo.lock
git commit -m "Release X.Y.Z: bump the workspace"
git push
```

Commit subject convention: `Release X.Y.Z: bump the workspace`.

### 5. Open the release PR → `development`

```sh
gh pr create --base development --head release/X.Y.Z \
  --title "Release X.Y.Z" --body "..."
gh pr edit <n> --add-assignee <you> --add-reviewer <reviewer>
```

### 6. Merge, then tag the merge commit

After approval, merge the PR, update local `development`, and tag **the merge
commit** (this is what every historical release tag points at):

```sh
gh pr merge <n> --merge
git checkout development && git pull
git tag vX.Y.Z <merge-sha>
git push origin vX.Y.Z
```

> The tag name carries a `v` prefix (`vX.Y.Z`); the workspace version does not
> (`X.Y.Z`). The workflows strip the `v` when comparing, so they must match on
> the numeric part.

### 7. Promote `development` → `main`

Open a second PR to bring `main` up to the tagged state:

```sh
gh pr create --base main --head development --title "Merge development → main (vX.Y.Z)"
```

Merging it runs `ci.yml` on `main` as a final green check.

## What the tag triggers

Pushing a `vX.Y.Z` tag fires **two** workflows. Both start with a `preflight`
job that validates the tag matches the `ed-agent` version in `Cargo.toml`, then
runs fmt + test + clippy. If the version and tag disagree, the release fails
here — this is the safety net for step 6.

### `release-sdk-rust.yml` → crates.io

- Dry-runs the workspace publish with `cargo-workspaces` (plain
  `cargo publish --dry-run` can't resolve the inter-crate path deps post-package).
- Publishes crates in dependency order to crates.io.
- `cargo workspaces plan --skip-published` makes re-runs safe: a run that dies
  mid-publish only publishes the crates still missing from the registry.

### `release-sdk-swift.yml` → XCFramework + `ed-swift`

- Builds the `EdFramework.xcframework` for Apple targets (~2h20m build;
  **not** cancel-in-progress — killing it leaves half-published state).
- Creates a GitHub Release with the xcframework zip + checksum as assets.
- Checks out `ondeinference/ed-swift`, rewrites `Package.swift` with the new
  release URL + checksum, copies the generated `ed_agent_ffi.swift`, commits,
  pushes to `ed-swift` `main`, and tags it `vX.Y.Z`.

## Manual / recovery runs

Both workflows have a `workflow_dispatch` with a `publish` boolean and a
`release_tag` input. Use this to re-publish an existing tag if a run failed
after tagging — e.g. the crates.io publish succeeded but the Swift build timed
out. Point it at the existing `vX.Y.Z` tag; the crates.io re-run skips anything
already on the registry.

## Toolchain

The Rust toolchain is pinned in `rust-toolchain.toml` (currently `1.94.0`).
Workflows read `channel` from it, so a toolchain bump is a normal source change,
not release-time config.

## Checklist

- [ ] On a `release/X.Y.Z` branch off `development`.
- [ ] Root `Cargo.toml`: `[workspace.package] version` + all `ed-*` path deps bumped.
- [ ] `cargo check --workspace` run (Cargo.lock updated).
- [ ] fmt / test / clippy all green locally.
- [ ] Commit `Release X.Y.Z: bump the workspace`.
- [ ] PR → `development`, reviewed, merged.
- [ ] `vX.Y.Z` tag on the merge commit, pushed.
- [ ] `development` → `main` PR opened/merged.
- [ ] Both tag workflows green (crates.io + Swift).
