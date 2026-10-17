<div align="center">
  <img alt="gray-context-budget" src="assets/icon.svg" width="120" height="120" />
  <h1>gray-context-budget</h1>
  <p><strong>One sidecar that keeps the context window in budget.</strong></p>
  <p>
    <a href="https://gray.alignment.id">Website</a> ·
    <a href="https://github.com/vstaln/gray-context-budget">Source</a> ·
    <a href="https://github.com/vstaln/gray">gray</a>
  </p>
  <p>
    <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-1c1c20?style=flat-square&labelColor=0a0a0b" /></a>
    <a href="https://www.rust-lang.org"><img alt="Built with Rust" src="https://img.shields.io/badge/built%20with-rust-1c1c20?style=flat-square&labelColor=0a0a0b&logo=rust&logoColor=d4a373" /></a>
    <img alt="gray plugin" src="https://img.shields.io/badge/gray-plugin-1c1c20?style=flat-square&labelColor=0a0a0b&color=7aa2f7" />
  </p>
</div>

<br/>

```bash
gray plugin install context-budget
```

`gray-context-budget` combines three plugins into a single sidecar process.
Each one manages context size or cost: truncate caps tool results, compact
auto-summarizes oversized context, and cacheopt watches for prompt-cache
prefix breaks. Each module keeps its own `~/.gray/<orig>/` state files and
slash commands. Existing setups (`~/.gray/truncate/`, `~/.gray/compact/`,
`~/.gray/cacheopt/state.json`, `GRAY_COMPACT_CHARS`) keep working after you
uninstall the standalone versions.

## Replaces

| Module | Former plugin | Hooks | Commands | Tools |
|---|---|---|---|---|
| truncate | `gray-truncate` | `tool/after` | `/truncate` | — |
| compact | `gray-compact` | `context/build` | `/compact` | — |
| cacheopt | `gray-cacheopt` | `context/build` `prompt/context` `event/notify` (`turn_end`) | `/cachestats` | — |

`context/build` is chained. compact runs first and may replace the oldest
70% of messages with a summary. cacheopt then fingerprints the list that
actually goes out. compact needs the `host.turn` capability
(`gray plugin capabilities context-budget --all`). Without it, compaction
fails open and the list goes out unchanged. `/context-budget` prints an
overview of all three modules.

## Build from source

```sh
cargo build --release
gray plugin check ./target/release/context-budget
```

## License

MIT
