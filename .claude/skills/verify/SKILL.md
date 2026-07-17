---
name: verify
description: Build and drive the jjscope TUI in a tmux pane to observe a change, instead of relying on unit tests alone.
---

# Verifying jjscope changes

jjscope is a TUI (ratatui) wrapping `jj`. Unit tests under `src/commander/`
cover the jj-wrapping logic well, but UI behavior (highlights, status
messages, popups) needs to be seen rendered.

## Build

```bash
cargo build
```

Binary lands at `target/debug/jjscope`.

## Set up a scratch jj repo

```bash
rm -rf /tmp/jjscope-verify-repo && mkdir -p /tmp/jjscope-verify-repo
cd /tmp/jjscope-verify-repo
jj git init --colocate --quiet
```

Build up whatever commit stack the feature needs with `jj describe -m ... --quiet`
and `jj new "@" --quiet` between file edits.

Gotcha: if you need `jj absorb` to split a working-copy diff across two
different ancestors, the two edited regions must be separated by at least
one **unchanged** line in the file — otherwise diff tooling merges them into
one hunk and absorb can't split it (it'll print "Nothing changed.").

## Launch and drive in tmux

Run in a dedicated tmux socket so it doesn't collide with any other session:

```bash
tmux -L jjscope-verify kill-server 2>/dev/null
tmux -L jjscope-verify new-session -d -s verify -x 220 -y 50 \
  "cd /tmp/jjscope-verify-repo && /home/sam-watson/code/jjscope/target/debug/jjscope"
sleep 1
tmux -L jjscope-verify capture-pane -t verify -p
```

Send keys with `tmux -L jjscope-verify send-keys -t verify '<keys>'`, then
`sleep` briefly and capture again. Use `capture-pane -p -e` to include ANSI
escape codes when you need to confirm a specific highlight color (e.g.
`grep -o '\[[0-9;]*m'` to list codes, or grep for a specific code like
`48;5;3m` for `Color::Yellow`).

Quit with `send-keys -t verify 'q'`, then `kill-server` and remove the scratch
repo.

## Notes

- The status message (top-right header) clears on the *next* keypress, not
  a timer — factor that into capture timing (capture before sending the next
  key).
- Default keybinds: `Shift+A` absorb, `Shift+I` insert-move, `i` insert-new,
  `Ctrl+R` rebase popup. See `docs/keybindings.md` for the full list.
