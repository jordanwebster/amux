# Profile switcher screenshots

These committed screenshots use the production TUI renderer with named,
deterministic fixtures. They show the switcher with Work selected, the Personal
fleet (`fix-auth`, `codex-retry`), and the Work fleet (`ship-invoices`,
`audit-ledger`). Work contains none of Personal's agents.

| Screen | Fixture | Capture |
| --- | --- | --- |
| Switcher | `profile-switcher` | [profile-switcher-dark.png](profile-switcher-dark.png) |
| Personal fleet | `fleet` | [fleet-personal-dark.png](fleet-personal-dark.png) |
| Work fleet | `fleet-switched` | [fleet-work-dark.png](fleet-work-dark.png) |

The `profiles` set uses a dark truecolor theme, a 120×40-cell viewport, and
vendored fonts. Each PNG is 1200×880 pixels. [manifest.json](manifest.json)
identifies the set, fixture names, dimensions and SHA-256 hashes.

From the repository root, regenerate and validate the committed set:

```sh
timeout 900 wt build
timeout 120 target/debug/amux-shot render-set profiles --out docs/screenshots/profiles
timeout 120 target/debug/amux-shot verify docs/screenshots/profiles
```

To compare a fresh render without replacing the committed files:

```sh
timeout 120 target/debug/amux-shot render-set profiles --out target/profile-screenshots
timeout 120 target/debug/amux-shot verify target/profile-screenshots
cmp docs/screenshots/profiles/manifest.json target/profile-screenshots/manifest.json
```

Verification decodes every PNG and checks its dimensions and hash; matching
manifests therefore identify matching image bytes. The images demonstrate
layout and fixture content. Runtime tests exercise switching and rejection of
late inventory, session, attachment and command results from the old profile:

```sh
timeout 900 wt test -- switcher
```
