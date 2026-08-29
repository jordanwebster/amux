# amux-shot

`amux-shot` renders deterministic PNG screenshots from the same pure
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
cargo run -p amux-shot -- verify target/shot
```

`--theme` accepts `dark`, `light`, or a YAML theme-file path. `--color`
accepts `truecolor` or `ansi` and defaults to truecolor.

The declared sets are `chat`, `agent-specific`, `gallery`, `scroll`, `copy`,
`collapse`, `themes`, `fleet`, and `all`. During development, a set whose
fixture has not landed yet exits with `UnknownState(name)`; this keeps the
eventual evidence contract visible without inventing placeholder pictures.

Each successful render appends its state, theme, colour mode, viewport, pixel
dimensions, filename, and SHA-256 digest to `manifest.json` beside the PNG.
`render-set` also records the completed set. `verify` recursively reads those
manifests, checks the hashes and fixed dimensions, and fully decodes every PNG,
so truncated files are rejected.

The JetBrains Mono files and `assets/OFL.txt` come from the
[JetBrains Mono project](https://github.com/JetBrains/JetBrainsMono). The
fallback and `assets/DejaVu-LICENSE.txt` come from the
[DejaVu Fonts project](https://github.com/dejavu-fonts/dejavu-fonts).
