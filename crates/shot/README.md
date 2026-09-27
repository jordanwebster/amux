# amux-shot

`amux-shot` renders deterministic PNGs of the terminal's chat vocabulary:
the same buffers the component goldens in `crates/tui/tests/golden`
compare as text and style maps, rasterized so a reviewer can see them. It
does not start a PTY, connect to a daemon, or inspect the local terminal.
Every cell is 10×22 pixels, drawn with vendored JetBrains Mono faces and a
DejaVu Sans fallback; both fonts carry their open-source licenses beside
the assets.

Run it through the declared task from the repository root:

```sh
just shot -- list
just shot -- render vocabulary --out target/amux-shot/vocabulary
just shot -- render vocabulary --theme dark --color ansi --out target/amux-shot/ansi
just shot -- verify target/amux-shot/vocabulary
```

`render vocabulary` writes one PNG per component and theme, a
`manifest.json` recording each file's size and SHA-256, and an `index.md`
naming every golden file with what it shows beside its PNGs. `verify`
checks every manifest below a directory: each PNG decodes, has the recorded
hash, and is exactly its recorded cell grid times the cell size.
