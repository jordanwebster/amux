# The terminal client

*For people using amux from a terminal, and developers changing `crates/tui` or the `amux` command line.*

The terminal client is the fleet and the chat in a full-screen terminal interface. Running `amux` with no verb
opens it. It is a library ([`crates/tui`](../crates/tui/src/lib.rs)) that the `amux` binary
([`crates/amux`](../crates/amux/src/main.rs)) calls, never a second executable, and it is built on the shared
client library described in [CLIENT.md](CLIENT.md): it reads the local runtime only through the session and fleet
drivers, composes the view values (fleet rows, chat rows, the ask card, the session strip, composer tokens) in its
own layout, and owns only panes, the scroll anchor, expanded rows, drafts and focus. Nothing it holds outlives the
process.

The same binary carries the command-line verbs, listed [at the end of this page](#command-line-verbs).

## Starting it

```sh
amux                        # the terminal client on the first profile
amux --profile work         # on a profile named by label or id
amux --config path/to.yaml  # with another installation config (or AMUX_CONFIG)
```

`amux` connects to the selected profile's local socket with the gRPC client, opens the fleet, and draws. If the
daemon is not running, start it with `amux server start` (see [SUPERVISOR.md](SUPERVISOR.md)).

The client redraws only when something changed: a key, a change from the fleet or the open chat, or a finished
task. It ticks on a clock only while something on screen moves with time: every 250 ms while the open agent is
working (the activity line's elapsed time) or its chat is still empty, and at the end of the quit guard or a
notice. See [the two draw loops](CLIENT.md#the-two-draw-loops).

## Screens

### The fleet

The fleet is home: the client opens on it, and every chat returns to it. It is a framed grid titled `amux`, with
one row per family head, loudest family first (needs you, working, starting, idle, exited), then most recently
active. Each row shows the attention mark, the agent's name, its kind (`claude`, `claude sdk`, `codex`), its host
and how that host is reached, the age of its last activity, its status word and what it is working on; narrower
terminals drop the working-on summary first, then the status word. Above the rows sit "N need you" when any do,
and the agent count. A banner line reports a reachability problem when there is one. The status bar shows the
connection, the host count and the keys that fit.

A family's children are hidden until the family is expanded; an expanded family lists its members indented under
their parents. Children an agent spawned through amux are agents of their own, with their own chats.

Overlays on the fleet:

- **New agent** (`n`): pick Claude (terminal), Claude (SDK) or Codex by `1`–`3` or the arrows and Enter. The agent
  works in the directory `amux` was started in, and its chat opens as soon as it exists.
- **Rename** (`r`): a one-line field; Enter applies, Esc cancels.
- **Stop** (`s`) and **delete** (`d`): a confirmation; `y` or Enter confirms, `n` or Esc cancels. Stop is not
  offered for an agent that has already exited. Delete removes the agent, its history and its children; children
  on hosts that cannot be reached stay listed on their own, and a notice says how many.

### Hosts

`h` opens the hosts overlay inside the fleet's frame: every trusted host whatever its presence, with how it is
reached now (`local` for this machine, `direct`, `relay`, `ssh`, or `away` and `offline`) and what stands in its
way: the host has stopped trusting this machine, this machine is signed out of its account, or the host is not
signed in (which matters only for reaching it through the relay). Hosts that local-network discovery currently
sees and that are not yet trusted are listed too. Esc, `q` or `h` closes it. Pairing, unpairing and signing in
happen from the command line ([`amux pair`](#command-line-verbs) and friends); [HOW_IT_WORKS.md](HOW_IT_WORKS.md)
explains hosts, pairing and the relay.

### The chat

Enter on a fleet row opens its chat. From top to bottom:

- **The header**: the agent's name, then faintly its project and, when it runs on another machine, its host. At
  the right, how much of its context is used (`41K / 200K`, in the warning ink when near the limit), `[Diff +a
  −b]` with the working tree's lines added and removed, which opens the review page, and `[Home]`, which goes
  back home; both brighten under the mouse. The totals are read when the chat opens and again each time a turn
  ends. The header says where the chat stands only when that is a problem: `exited` with its cause, `catching
  up` before the first CaughtUp, `reconnecting` while the local runtime is being reconnected, `refreshing` while
  a rebuilt history is on its way, or why the host is away (not current, this machine signed out, or the host
  has stopped trusting this machine). Working and asks show in the feed.
- **The family line**, for an agent with a parent or children: the parent, the number of subagents and how many
  need you.
- **The feed**, read one turn at a time, laid out upward from the newest or from wherever the reader scrolled:
  - your message is a tinted block with the time at its right;
  - everything the agent wrote is in one reading ink, to at most 100 columns a line on a wide terminal; code
    and paths in it wear the code colour, the one colour kept for them and never the accent's; its thinking is
    not drawn;
  - each stretch of tool steps between two pieces of its text folds to one faint line of what the steps did
    (`▸ 3 commands · 2 edits · 4 reads`). Clicking it, or `<leader> o` on it, opens the stretch to one line per
    step, with consecutive reads and searches merged into one; clicking a step opens its detail (a command's
    output, an edit's patch, a call's result);
  - a failure the turn ended without fixing stays under its folded stretch in red; one fixed later in the turn
    folds away with the rest;
  - while the agent is at a stretch, its newest three steps show as they happen, the current one bright, with a
    line for the ones above them, and the stretch folds once the agent writes again;
  - a turn ends with a faint `Worked 6m`.

  Asks, errors, boundaries and the other rows [CHAT_VOCABULARY.md](CHAT_VOCABULARY.md) lists keep their own
  drawing for now.
- **The ask card**, when the agent is waiting on you, docked where the composer was; the draft stays underneath
  it.
- **The composer**, boxed, with `Model (effort) · mode` on its bottom edge; above it a quiet line while the
  agent works, the session strip, any foot card (a sign-in problem or a usage block) and the tray of queued and
  unconfirmed prompts; one blank line below it, a line of keys for the composer's current mode, all at the left
  with `shift+tab mode` last.

Design variant 1 (F3 in [the lab](#the-lab)) draws the chat as it was before this redesign: a header with
the kind, host, model and mode, every row drawn in full, and an unboxed composer.

The composer takes a draft at any time. Enter sends only while the chat is current and the agent is live; while
the agent works, Enter queues the prompt behind the running turn. When the agent has exited, Enter resumes it with
the draft as its first prompt. A draft typed while the chat is catching up or its host is away is kept, and the
key line says sending waits.

An empty chat says it is loading after 300 ms. Scrolling up keeps the reader's place while rows arrive below; the
client asks for older rows by itself when fewer than a page (40 rows) is held above the screen.

**Attachments.** Ctrl+V reads the clipboard: an image or a copied file is stored with the agent and inserted at
the cursor as an attachment chip; text is pasted. A paste of eight lines or more, or of 1000 characters or more,
becomes a text attachment instead of flooding the draft. One Backspace removes a chip with its attachment.
[ATTACHMENTS.md](ATTACHMENTS.md) covers what happens to attachments after that.

**The reader.** `f` on an ask card opens the whole diff, plan, command, arguments or question previews full
screen; `j`/`k`, the arrows, Page Up/Down, Space, `g`/`G` and Home/End scroll it, and Esc or `q` closes it.

### The review page

`<leader> r` asks the agent's host for its working-tree diff and opens it as a full-screen review page, frozen as
it was when the page opened. You move line by line, leave comments on lines or on whole files, and go back to the
chat; the comments ride in the draft as one review attachment, which the page keeps in step as comments are saved
and deleted. `<leader> r` again with a review already in the draft returns to that page rather than reading a
fresh diff. The page opens only while the chat is current, and says so when the working tree has no changes.
[ATTACHMENTS.md](ATTACHMENTS.md) describes the review attachment and how the agent receives it.

### Settings

The terminal client has no settings sheet. Shift+Tab moves the agent to its next mode where it has one to move to
(terminal Claude cycles its own permission modes; for the others the next offered mode that still asks before
acting), and the composer's bottom edge shows the model, effort and mode. Terminal Claude's model and effort change by typing its
own command in the composer. The phone's settings sheet offers every model, effort and mode the agent reports.

The terminal's own appearance and leader key come from the installation config file
(`~/.config/amux/config.yaml` unless `--config` or `AMUX_CONFIG` names another), read by
[`crates/settings`](../crates/settings/src/lib.rs):

```yaml
keybinds:
  leader: ctrl+a      # ctrl+<a-z>
ui:
  theme: terminal     # terminal, dark, light, or a path to a YAML theme file
  color: auto         # auto, truecolor or ansi
```

`theme: terminal` asks the terminal for its own colours at startup and derives the palette from them, falling
back to the shipped dark palette when the terminal does not answer. A theme file's path is resolved beside the
config file. `color: auto` reads `COLORTERM` and `TERM` to choose truecolor or ANSI, and `NO_COLOR` turns colour
off.

## Keys

`?` shows the key help from the fleet, or from the chat while the composer is empty and nothing else has the
keys; any key closes it.

### Everywhere

| Key | Action |
|---|---|
| Ctrl+C | Clears the field that has the keys, if it holds text (Ctrl+Y brings it back). On nothing, press it twice within three seconds to quit. |
| Ctrl+A, then a key | A leader chord; see [the leader key](#the-leader-key). |

### Fleet

| Key | Action |
|---|---|
| ↑ ↓ or `k` `j` | Move the selection |
| Home End or `g` `G` | First or last row |
| Enter | Open the chat |
| `o` or Ctrl+Enter | Attach to the agent's own terminal (agents on this machine only) |
| `z`, Space or Tab | Show or hide a family's agents |
| `n` `r` `s` `d` | New agent, rename, stop, delete |
| `h` | The hosts overlay |
| `q` | Quit |

### Chat

| Key | Action |
|---|---|
| Enter | Send; queue while the agent works; resume an exited agent |
| Ctrl+J or Shift+Enter | New line |
| Ctrl+V | Attach an image or file from the clipboard |
| ↑ on the first line | Into the tray of queued and unconfirmed prompts |
| Ctrl+X | Stop the turn; the agent stays live and idle |
| Shift+Tab | The agent's next mode, where it has one |
| Page Up, Page Down, mouse wheel | Scroll |
| Ctrl+Home, Ctrl+End | The oldest held row; follow the newest |
| Esc | Close the reader, then clear focus, then follow the newest. Esc never answers an ask and never stops the agent. |

The draft editor takes readline keys: Ctrl+B and Ctrl+F move by character, Ctrl+Left and Ctrl+Right by word,
Ctrl+E and End to the line's end, Home to its start, Ctrl+P and Ctrl+N (or ↑ and ↓) between lines; Ctrl+W,
Ctrl+U and Ctrl+K kill the word before, the line before and the line after the cursor; Ctrl+D deletes forward;
Ctrl+Y yanks the last kill back.

In the tray, ↑ and ↓ move, Esc leaves; on a queued prompt Enter or `s` sends it now (steering it into the running
turn) and `w`, Delete or Backspace withdraws it back into the composer; on an unconfirmed prompt `r` resends and
`d` discards; on a rejected prompt `e` puts its words back in the composer and `d` discards it.

### Ask card

| Key | Action |
|---|---|
| ↑ ↓, `1`–`9` | Select a choice |
| Enter | Confirm it. A choice that takes a note (a Claude denial, a plan sent back) opens the note field first; Enter sends, Esc goes back. |
| `f` | Open the whole diff, plan, command, arguments or previews in the reader |
| Ctrl+X | Stop the turn |

Stop the turn always follows the choices, and an ask this client cannot answer also offers to attach the agent's
own terminal when it is on this machine. For questions, Enter picks and moves on, Space toggles a
choice in a multi-select question, Tab and Shift+Tab (or → and ←) move between questions, and typing on
"Something else…" starts a typed answer. One pick-one question without previews answers on Enter; every other
shape ends on a review screen, where `1`–`9` goes back to a question, `n` adds a note where the agent takes one,
and Enter sends. For a form, ↑ and ↓ move through the fields and then the choices, Enter edits a field, and Space
flips a toggle or steps a choice. After an answer the card shows one sending line; if the connection dropped
before a reply, `r` resends and `d` discards.

### Review page

| Key | Action |
|---|---|
| `j` `k`, ↓ ↑ | Next or previous line |
| Page Down, Space, Page Up | A screen at a time |
| `g` `G`, Home End | First or last line |
| `J` `K` | Next or previous hunk |
| `]` `[` | Next or previous file |
| `c` | Comment on the line (on a file header, the whole file) |
| Enter | Edit the newest comment here, or write one |
| `d` | Delete the newest comment here |
| `q`, Esc | Back to the chat |

While writing a comment, Enter saves it, Ctrl+J adds a line, and Esc cancels.

## The leader key

The leader is a control key, Ctrl+A unless `keybinds.leader` names another `ctrl+<letter>`. Pressed on its own it
waits for one more key, and the footer lists the chords. In the chat:

| Chord | Action |
|---|---|
| `<leader> s` | Back to the fleet; the chat closes |
| `<leader> d` | Leave for the shell; agents keep running |
| `<leader> k`, `<leader> j` | Focus an older or newer row |
| `<leader> o` | Open or close the focused row, or its run |
| `<leader> y` | Copy the focused row (through the terminal's OSC 52 clipboard) |
| `<leader> r` | Review the working tree; comments go in the draft |
| `<leader> n` | Open the next agent in this agent's family, wrapping |
| `<leader> t` | Attach to the agent's own terminal, when it is on this machine |

In the fleet the leader has no chords. During raw attach the leader is read out of the bytes you type:
`<leader> d` detaches to the shell, `<leader> s` opens the fleet over the attached agent, and a leader followed by
any other key goes to the agent exactly as typed. The leader is recognised both as its control byte and in the
form terminals send when an agent has switched on the kitty keyboard protocol.

## Raw attach

Raw attach hands the terminal to an agent's own interface: terminal Claude's own screen, or a Codex terminal view
of the agent's thread. It is reached with `amux attach <agent>`, with `o` or Ctrl+Enter in the fleet, with
`<leader> t` in a chat, or from an ask card this client cannot answer.

Terminal bytes never travel over the wire. The client finds the agent's directory by convention,
`<profile directory>/agents/<agent id>/`, and connects straight to its `pty.sock`; the daemon is not involved.
[AGENT_PROCESS.md](AGENT_PROCESS.md) describes the socket, including how terminal Claude's screen is replayed to a
late attacher and how each Codex attach gets its own view of the thread. Two refusals follow, each pointing at the
chat, which reaches every agent:

- **An agent on another host.** Its terminal is on another machine. `amux attach` says so and suggests opening its
  chat with `amux` and Enter; the fleet shows the same notice.
- **Headless Claude** (`claude_sdk`). It has no terminal.

While attached, typing and resizes go to the agent. How it ends:

| Ending | `amux attach` | From the terminal client |
|---|---|---|
| `<leader> d` | Prints `[detached from <name>]` and returns to the shell | Leaves the client for the shell |
| `<leader> s` | Opens the fleet over the attached agent | Returns to the fleet |
| The agent ends the connection | Prints `[<name>: <why>]` or `[<name> ended]` and returns to the shell | Leaves the client for the shell |

Only the fleet chord comes back to the fleet. Leaving for the fleet keeps the connection and a model of the
agent's screen, so attaching to the same agent again repaints it where it was rather than starting over; picking
another agent opens a second connection. The terminal is restored whichever way attach ends, including the modes
the agent's interface switched on.

## Command-line verbs

Every verb takes the global `--config <path>` (or `AMUX_CONFIG`) and `--profile <label or id>`. An agent is named
by its name or its id.

| Verb | What it does |
|---|---|
| `amux` | Opens the terminal client. |
| `amux ls` (`list`) | Lists the agents with children beneath their parent: name, short id, kind and state. |
| `amux create <kind>` (`new`) | Starts an agent: `claude_pty` (also `claude`), `claude_sdk` or `codex`, with `--name`, `--cwd` (the current directory otherwise), `--model`, `--prompt` for its first prompt, and extra arguments for Claude after `--`. |
| `amux attach <agent>` | Hands this terminal to the agent's own interface; see [raw attach](#raw-attach). |
| `amux send <agent> <text…>` | Sends a prompt and prints whether the agent took it. |
| `amux stop <agent>` | Stops the agent's process: `--mode graceful` (the default: finish the turn, then exit), `abort` (cancel the turn and exit) or `kill` (end the process group at once). |
| `amux resume <agent> [text…]` | Starts an exited agent again, with an optional first prompt. |
| `amux rename <agent> <name>` | Renames an agent. |
| `amux delete <agent>` (`rm`) | Deletes an agent, its history and its children. |
| `amux dump [agents…]` | Writes a debug bundle for the named agents, or all of them, with an optional `--reason`, and prints its path. See [DEBUGGING.md](DEBUGGING.md). |
| `amux pair [target]` | Pairs with another host found nearby by name, an address, or `user@host` over SSH. With no target this host opens pairing mode and shows a PIN, or a QR code for the phone with `--qr` (`--print-link` also prints its link); `--link` pairs from an `amux://pair` link; `--cancel` closes pairing mode. |
| `amux peers` | Lists the paired hosts and the hosts found nearby. |
| `amux unpair <peer>` | Stops trusting a paired host, asking first unless `--force`. |
| `amux login` | Signs the profile in to an amux account so its hosts reach each other through the relay (`--cloud-url` for another account service). |
| `amux logout` | Signs the profile out; its agents and paired hosts stay. |
| `amux profiles` | Lists the installation's profiles. |
| `amux profile list\|create [label]\|rename <profile> <name>\|delete <profile>` | Manages the installation's profiles. |
| `amux init` | Sets up this install, including whether amux starts at login (`--login-item yes\|no`, asked when omitted). |
| `amux update` | Asks the supervisor to install the channel's release now, even one that was rolled back here. |
| `amux config channel stable\|preview` | Chooses which releases the supervisor follows. |
| `amux server start` | Starts amux detached from the terminal: the supervisor where the install has one, the daemon otherwise. With `--cloud` it runs the cloud relay in the foreground instead. |
| `amux server stop` | Stops amux cleanly, the supervisor first where one runs; agents keep running. |
| `amux supervise` | Runs the daemon under a supervisor that restarts it and, under `updates: auto`, installs releases. See [SUPERVISOR.md](SUPERVISOR.md). |

Hidden subcommands exist for amux's own processes rather than for people: `daemon` (the daemon in the
foreground), `agent <dir>` (the agent process), `mcp <dir>` (the tool server an agent's provider launches),
`hooks claude` (terminal Claude's hook command), and `pair-recv` and `relay` (the far end of SSH pairing and a
peer's link over SSH).

## Tests and evidence

| What | Where | Run |
|---|---|---|
| Behaviour: keys, effects, layout, paging, the ask card, the tray, the review page | `crates/tui/src/tests.rs` | `just test-tui` |
| Component goldens in light and dark: every row kind, the activity line, each ask, the strip, the tray, the composer states, the review page | `crates/tui/tests/golden.rs`, `crates/tui/tests/golden/` | `just test-tui`; rewrite with `UPDATE_GOLDENS=1 just test-tui` |
| Whole frames from served hosts: the fleet at several widths, the hosts overlay, chats mid-call and through a rewind | `crates/tui/tests/frames.rs` | `just test-tui` |
| PNG renderings of the components | [`crates/shot`](../crates/shot/README.md) | `just shot -- render vocabulary --out DIR` |
| Raw attach, the CLI and the binary against real daemons | `crates/amux/tests/` | `just test-crate amux` |
| Journeys through the real terminal client | `journeys/`, `scripts/terminal-journey.py` | `just journey terminal <name>` |
| The lab's scenarios drawn at two sizes, and its fake runtime answering a prompt and an ask | `crates/tui-lab` | `just test-crate tui-lab` |

[TESTING.md](TESTING.md) places these in the suite catalogue.

## The lab

The lab ([`crates/tui-lab`](../crates/tui-lab/src/main.rs)) runs this client over a scripted fake runtime instead
of a daemon, for trying designs and walking through flows. The fake runtime implements the same `Client` the
daemon's socket does and changes the way a daemon would: a sent prompt is reflected and answered, an answered ask
closes and the agent carries on, a created agent starts and answers its first prompt, and stop, resume, rename,
delete, the working-tree diff and attachments all work against it. No provider runs and nothing is persisted.

```sh
just lab rich-chat        # open a scenario; relaunch at the same place on every change
just lab                  # resume the last scenario and place
just lab-list             # the scenarios
just lab-render asks --keys enter --size 120x40 --size 80x24
```

**Scenarios** are YAML files in `crates/tui-lab/scenarios/`, read at launch: the hosts and their presence, the
agents (kind, host, family, phase, model and mode, context, usage, tasks, queue, a working-tree diff) with their
transcripts written as one-key entries (`- user:`, `- say:`, `- read:`, `- bash:`, `- edit:`, `- subagent:`,
`- ask:` with a permission, question, plan, form, link or unanswerable body, `- turn:`), what each agent says
back to a prompt (`reply:`), and a timeline of beats that play in real time: an agent continues, a new agent
appears, a host drops away and returns. Entries after an ask wait for its answer. `src/scenario.rs` documents
every field.

**Relaunching.** `just lab` runs `tui-lab watch`, which polls the client's crates and the scenarios. A code change
rebuilds in the background while the old lab stays on screen, then the lab saves its place and the new build
reopens it there, in about two seconds after an edit in `crates/tui`. A scenario change relaunches without a
build. A failed build leaves the old lab running with a notice naming the first error; the whole log is
`notes/tui-lab/build.log`. The place is the scenario, how many timeline beats have fired, the open chat, the
fleet's selection, the draft and the scroll anchor. What was done against the fake runtime is not kept: the world
is rebuilt from the scenario and those beats.

**Lab keys**, layered over the client's own:

| Key | What it does |
|---|---|
| F2 or Ctrl+] f | Capture this screen with a one-line note: text, PNG, note and place go to `notes/tui-lab/feedback/<time>/` |
| F3 or Ctrl+] v | Cycle the design variant (`tui::variant`), for renderers comparing designs; variant 0 is the current design, and variant 1 draws the old framed home in place of [`home.rs`](../crates/tui/src/home.rs) |
| Ctrl+] r | Restart the scenario from its beginning |

`tui-lab render` draws frames without a terminal: the scenario after `--step N` beats and a sequence of keys
and mouse events (`--keys "down enter 'fix it' enter"`, with named keys, `C-x` and `C-enter` for Ctrl, and
`hover:X,Y` or `click:X,Y` for the mouse at a zero-based cell), in `--variant N`, at each `--size`, to text and
PNG. By default it draws in the colours the person's terminal reported the last time the lab ran in it (kept in
`notes/tui-lab/terminal-colors.json`), so frames look like theirs; `--theme sample` draws as if a fixed dark
terminal had reported its colours, and `dark` and `light` are amux's own palettes, drawn as for a terminal that
does not answer. Scripts
play at once there, so a frame does not wait on timing.
