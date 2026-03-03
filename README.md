<div class="title-block" style="text-align: center;" align="center">

# jjscope - A TUI for [Jujutsu](https://github.com/jj-vcs/jj)

Built in Rust with Ratatui. Interacts with `jj` CLI.

</div>

## Features

- Log
  - Scroll through the jj log and view change details in side panel
  - Create new changes from selected change with `n`
  - Insert a new change, or move the selected change, between marked changes with `i`/`I`
  - Edit changes with `e`/`E`
  - Describe changes with `d`
  - Abandon changes with `a`
  - Absorb a change's diff into its mutable ancestors with `A`
  - Generate a new change id (resolve divergence) with `c`/`C`
  - Toggle between color words and git diff with `p`
  - See different revset with `r`
  - Set a bookmark to selected change with `b`
  - Fetch/push with `f`/`p`
  - Squash changes with `s`/`S`: pick up, then pick the destination
  - Yank change ID/revision to the system clipboard with `y`/`Y`
- Files
  - View files in current change and diff in side panel
  - See a change's files from the log tab with `Enter`
  - View conflicts list in current change
  - Toggle between color words and git diff with `w`
  - Untrack file with `x`
- Bookmarks
  - View list of bookmarks, including from all remotes with `a`
  - Create with `c`, rename with `r`, delete with `d`, forget with `f`
  - Track bookmarks with `t`, untrack bookmarks with `T`
  - Create new change with `n`, edit change with `e`/`E`
  - Push a single bookmark with `p`
- Command log: View every command jjscope executes
- Config: Configure jjscope with your jj config
- Command box: Run jj commands directly in jjscope with `:`
- Help: See all key mappings with `?`

## Setup

Make sure you have [`jj`](https://martinvonz.github.io/jj/latest/install-and-setup) installed first.

- With [`cargo binstall`](https://github.com/cargo-bins/cargo-binstall): `cargo binstall jjscope`
- With `cargo install`: `cargo install jjscope --locked` (may take a few moments to compile)
- With pre-built binaries: [View releases](https://github.com/sswatson/jjscope/releases)

To build and install a pre-release version: `cargo install --git https://github.com/sswatson/jjscope.git --locked`

## Configuration

You can optionally configure the following options through your jj config:

- `jjscope.highlight-color`: Changes the highlight color. Can use named colors. Defaults to `#323264`
- `jjscope.diff-format`: Change the default diff format. Can be `color-words` or `git`. Defaults to `color_words`
  - If `jjscope.diff-format` is not set but `ui.diff.format` is, the latter will be used
- `jjscope.diff-tool`: Specify which diff tool to use by default
  - If `jjscope.diff-tool` is not set but `ui.diff.tool` is, the latter will be used
- `jjscope.bookmark-template`: Change the bookmark name template for generated bookmark names. Defaults to `'push-' ++ change_id.short()`
  - If `jjscope.bookmark-template` is not set but `templates.git_push_bookmark` is, the latter will be used
- `jjscope.layout`: Changes the layout of the main and details panel. Can be `horizontal` (default) or `vertical`
- `jjscope.layout-percent`: Changes the layout split of the main page. Should be number between 0 and 100. Defaults to `50`

Example: `jj config set --user jjscope.diff-format "color-words"` (for storing in [user config file](https://martinvonz.github.io/jj/latest/config/#user-config-file), repo config is also supported)

## Usage

To start jjscope for the repository in the current directory: `jjscope`

To use a different repository: `jjscope --path ~/path/to/repo`

To start with a different default revset: `jjscope -r '::@'`

## Key mappings

See all key mappings for the current tab with `?`.

### Basic navigation

- Quit with `q`
- Change tab with `1`/`2`/`3` or with `h`/`l`
- Scrolling in main panel
  - Scroll down/up by one line with `j`/`k` or down/up arrow
  - Scroll down/up by half page with `J`/`K` or down/up arrow
- Scrolling in details panel
  - Scroll down/up by one line with `Ctrl+e`/`Ctrl+y`
  - Scroll down/up by a half page with `Ctrl+d`/`Ctrl+u`
  - Scroll down/up by a full page with `Ctrl+f`/`Ctrl+b`
- Open a command popup to run jj commands using `:` (jj prefix not required, e.g. write `new main` instead of `jj new main`)

### Log tab

- Select current change with `@`
- View change files in files tab with `Enter`
- Display different revset with `r` (`jj log -r`)
- Change details panel diff format between color words (default) and Git (and diff tool if set) with `w`
- Toggle details panel wrapping with `W`
- Create new change after highlighted change with `n` (`jj new --no-edit`)
  - `@` stays where it is; the cursor moves to the new change, and `e` edits into it
  - Create new change and describe with `N`
- Insert a new change between other changes with `i` (`jj new --no-edit -A -B`; `@` stays where it is)
  - Marking two changes where one is an ancestor of the other inserts between them immediately —
    the assignment is inferred, since the reverse would be a cycle
  - Otherwise the marked changes (or the highlighted one) become the after-anchors; after pressing
    `i`, pick the before-anchors (`Space` to mark several, or just point at one) and press `Enter`
  - Press `Esc` to cancel
- Move the highlighted (or marked) change between other changes with `I` (`jj rebase -r -A -B`)
  - After pressing `I`, pick the after-anchors, press `Enter`, pick the before-anchors, and
    press `Enter` again
- Edit highlighted change with `e` (`jj edit`)
  - Edit highlighted change ignoring immutability with `E` (`jj edit --ignore-immutable`)
- Abandon a change with `a` (`jj abandon`)
- Simplify parents of the marked/highlighted change(s) with `x` (`jj simplify-parents -r`)
  - Simplify the change(s) and all their descendants with `X` (`jj simplify-parents -s`)
- Absorb the highlighted change's diff into its mutable ancestors with `A` (`jj absorb --from`)
  - Until the next keypress, the log marks the revisions that actually received hunks with `★`
    and the revisions that were only rebased as a consequence with `☆`
- Resolve all conflicts in the highlighted change with `v`/`V` (`jj resolve --tool :theirs`/`:ours`)
  - `v` keeps the version from the revision that was moved by the conflict-introducing operation
    (labeled "rebased revision" or "squashed revision" in jj's conflict markers)
  - `V` keeps the version from the operation's destination (labeled "rebase destination" or
    "squash destination")
  - Each conflicted file takes the chosen side's entire content, i.e. exactly what that side had for the file before the conflict
- Generate a new change id for the highlighted change with `c` (`jj metaedit --update-change-id`), useful for resolving divergence
  - Generate a new change id ignoring immutability with `C` (`jj metaedit --update-change-id --ignore-immutable`)
- Describe the highlighted change with `d` (`jj describe`)
  - Save with `Ctrl+s`
  - Cancel with `Esc`
- Set a bookmark to the highlighted change with `b` (`jj bookmark set`)
  - Scroll in bookmark list with `j`/`k`
  - Create a new bookmark with `c`
  - Use auto-generated name with `g`
- Squash changes with `s` (`jj squash --from --into`): press `s` to pick up the marked changes
  (or the highlighted one), then pick the destination and press `Enter`
  - The cursor starts on the parent, so `s` then `Enter` squashes into the parent (like bare `jj squash`)
  - Squash ignoring immutability with `S` (`jj squash --ignore-immutable`)
- Rebase changes with `r` (`jj rebase -r`/`-s`): press `r` to pick up the marked changes
  (or the highlighted one), then edit the parent set and press `Enter`
  - The picked-up change's current parents appear marked with `✚`; `Space` toggles any
    change in or out of the parent set, so parents can be added and removed in one go
    (e.g. adding/dropping branches from a megamerge)
  - If the parent set is left untouched, `Enter` rebases onto the highlighted change
    instead — the plain "move it there" gesture
  - Press `r` again during the gesture to toggle whether descendants come along
    (`jj rebase -s` vs `-r`); the title shows which mode is active
- Rebase a whole branch with `B` (`jj rebase -b`): pick up a change on the branch, press
  `B`, then pick the destination(s) and press `Enter`
  - Which commits get new parents (the branch roots) depends on the destination, so
    there is no parent set to edit here — it's a plain destination pick
- Git fetch with `f` (`jj git fetch`)
  - Git fetch all remotes with `F` (`jj git fetch --all-remotes`)
- Git push with `p` (`jj git push`)

### Files tab

- Select current change with `@`
- Resolve the selected file's conflict with `v`/`V` (`jj resolve --tool :theirs`/`:ours`)
  - `v` keeps the rebased/squashed revision's version; `V` keeps the rebase/squash destination's version
- Change details panel diff format between color words (default) and Git (and diff tool if set) with `w`
- Toggle details panel wrapping with `W`

### Bookmarks tab

- Filter bookmarks by name with `/`
- Show bookmarks with all remotes with `a` (`jj bookmark list --all`)
- Create a bookmark with `c` (`jj bookmark create`)
- Rename a bookmark with `r` (`jj bookmark rename`)
- Delete a bookmark with `d` (`jj bookmark delete`)
- Forget a bookmark with `f` (`jj bookmark forget`)
- Track a bookmark with `t` (only works for bookmarks with remotes) (`jj bookmark track`)
- Untrack a bookmark with `T` (only works for bookmarks with remotes) (`jj bookmark untrack`)
- Change details panel diff format between color words (default) and Git (and diff tool if set) with `w`
- Toggle details panel wrapping with `W`
- Create a new change after the highlighted bookmark's change with `n` (`jj new`)
  - Create a new change and describe with `N` (`jj new -m`)
- Edit the highlighted bookmark's change with `e` (`jj edit`)
  - Edit the highlighted bookmark's change ignoring immutability with `E` (`jj edit --ignore-immutable`)
- Push the highlighted bookmark with `p` (`jj git push -b <bookmark>`)

### Command log tab

- Select latest command with `@`
- Toggle details panel wrapping with `W`

### Configuring

Keys can be configured

```toml
[jjscope.keybinds.log-tab]
save = "ctrl+s"
```

See more in [keybindings.md](docs/keybindings.md)

## Related Projects

 * [jjscope.nvim](https://github.com/sswatson/jjscope.nvim) -- A Neovim plugin that provides a floating window interface for jjscope

## Development

### Setup

1. Install Rust and
2. Clone repository
3. Run with `cargo run`
4. Build with `cargo build --release` (output in `target/release`)
5. You can point it to another jj repo with `--path`: `cargo run -- --path ~/other-repo`

### Logging/Tracing

jjscope has 2 debugging tools:

1. Logging: Enabled by setting `JJSCOPE_LOG=1` when running. Produces a `jjscope.log` log file
2. Tracing: Enabled by setting `JJSCOPE_TRACE=1` when running. Produces `trace-*.json` Chrome trace file, for `chrome://tracing` or [ui.perfetto.dev](https://ui.perfetto.dev)

## Release process

Create a release commit using [cargo
release](https://github.com/crate-ci/cargo-release), e.g. `cargo release
minor`, then open a PR and after it has been merged, create a GitHub release
for that commit. The "Release" workflow will fill in the description from the
changelog, generate and attach the binaries and publish the new version to
crates.io. That's it.

## Acknowledgements

jjscope is a fork of blazingjj (itself a fork of lazyjj, started by Charles Crete in 2023).
