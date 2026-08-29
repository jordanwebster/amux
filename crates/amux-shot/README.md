# amux-shot

`amux-shot` renders deterministic PNG screenshots and animated wheel recordings from the same pure
`amux-tui::render` boundary used by the text goldens. It does not start a PTY,
connect to a daemon, or inspect the local terminal. Every capture uses a
120-column by 40-row ratatui `TestBackend`, 10×20-pixel cells, and vendored
JetBrains Mono faces with a DejaVu Sans fallback. Both fonts carry their
open-source licenses beside the assets.

The crate is a workspace member but is intentionally outside the workspace's
default members. Run it explicitly from the repository root:

```sh
cargo run -p amux-shot -- list
cargo run -p amux-shot -- render claude-idle --out target/shot/claude-idle.png
cargo run -p amux-shot -- render claude-idle --theme light --color ansi \
  --out target/shot/claude-idle-light-ansi.png
cargo run -p amux-shot -- render-set chat --out target/shot/chat
cargo run -p amux-shot -- record-scroll claude --out target/shot/scroll
cargo run -p amux-shot -- record-scroll codex --out target/shot/scroll
cargo run -p amux-shot -- verify target/shot
```

To reproduce the complete review bundle in one command, use the repository
wrapper:

```sh
scripts/tui-evidence target/tui-evidence
```

The wrapper renders every declared set, records both agents' wheel sessions,
captures the command help and fixture list, proves byte-for-byte repeatability,
and records the debug and release paint benchmarks plus the theme-loader and
OSC 52 tests. It finishes by recursively verifying every PNG manifest below
the output directory and exits non-zero if rendering, recording, testing,
repeatability, or verification fails. Existing unrelated files in the output
directory are left in place.

`--theme` accepts `dark`, `light`, or a YAML theme-file path. `--color`
accepts `truecolor` or `ansi` and defaults to truecolor.

`record-scroll <claude|codex>` builds that agent's 1,000-entry long-feed
fixture, captures the initial frame, then routes twelve wheel-up and twelve
wheel-down mouse events through the production chat handler. It writes a
25-frame `<agent>-wheel.gif` and updates the shared `events.json` with every
event and resulting scroll state. Running both commands into one directory
preserves both recordings. The `scroll` render set supplies the matching
following and scrolled-back PNGs for both agents.

The declared PNG sets are `chat`, `agent-specific`, `gallery`, `scroll`, `copy`,
`collapse`, `themes`, `fleet`, and `all`.

Each successful render records its state, theme, colour mode, viewport, pixel
dimensions, filename, and SHA-256 digest in `manifest.json` beside the PNG. A
re-render of the same filename replaces its prior record, and a repeated
`render-set` replaces the receipt with the same set name. `verify` recursively
reads those manifests, checks the hashes and fixed dimensions, and fully
decodes every PNG, so truncated files are rejected.

The JetBrains Mono files and `assets/OFL.txt` come from the
[JetBrains Mono project](https://github.com/JetBrains/JetBrainsMono). The
fallback and `assets/DejaVu-LICENSE.txt` come from the
[DejaVu Fonts project](https://github.com/dejavu-fonts/dejavu-fonts).
