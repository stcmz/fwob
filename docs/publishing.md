# Publishing

The workspace publishes four crates in dependency order:

1. `fwob-core`
2. `fwob-v1`
3. `fwob-v2`
4. `fwob`

## Automated Releases

GitHub Actions publishes tagged releases. The PowerShell helper performs the
version bump, internal dependency updates, validation, commit, tag, and push:

```powershell
.\scripts\release.ps1 patch
.\scripts\release.ps1 minor
.\scripts\release.ps1 major
```

The release workflow verifies that the tag matches all four package versions,
publishes crates in dependency order, builds Windows, Linux, and macOS
binaries, generates SHA-256 checksums, and creates a GitHub Release.

CI verifies the workspace against stable Rust on Windows, Linux, and macOS and
checks the declared minimum supported Rust version, currently 1.85.

The repository must define `CARGO_REGISTRY_TOKEN`. The workflow uses the
`crates-io` GitHub environment so repository owners can add required reviewers
for the irreversible crates.io publication step. Workflow reruns skip crate
versions that are already present on crates.io.

## Manual Fallback

Authenticate once:

```bash
cargo login
```

For the initial release, publish each crate separately:

```bash
cargo publish -p fwob-core
cargo publish -p fwob-v1
cargo publish -p fwob-v2
cargo publish -p fwob
```

Wait for each newly published crate to appear in the crates.io index before
publishing a crate that depends on it. Run the full test suite and
`cargo build --release` before publishing.

The CLI is installed with:

```bash
cargo install fwob
```
