# Publishing

The workspace publishes four crates in dependency order:

1. `fwob-core`
2. `fwob-v1`
3. `fwob-v2`
4. `fwob`

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
