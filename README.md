<div class="title-block" style="text-align: center;" align="center">

# jjscope - A TUI for [Jujutsu](https://github.com/jj-vcs/jj)

Built in Rust with Ratatui. Interacts with `jj` CLI.

</div>

## Features

- Log
  - Scroll through the jj log and view change details in side panel
  - Create new changes from selected change with `n`, or with `N` to leave `@` where it is
  - Mark anchors with `Space` (goes after) and `i` (goes before) to splice with `n`/`r` instead
    of appending
  - Edit changes with `e`/`E`
  - Describe changes with `d`
  - Abandon changes with `a`
  - Absorb a change's diff into its mutable ancestors with `A`
  - Generate a new change id (resolve divergence) with `c`/`C`
  - Toggle the details panel between a `--stat` summary (default), color words, and git diff with `p`
  - Mark the revisions matching a revset with a gutter bar, leaving the graph intact — set it
    alongside the log's own revset with `Ctrl+r`, or mark the revisions touching a path with `T`
  - See different revset with `r`
  - Set a bookmark to selected change with `b`
  - Set a tag on the selected change with `t`
  - Fetch/push with `f`/`p`
  - Squash changes with `s`/`S`: pick up, then pick the destination
  - Browse the whole repo at the selected revision in your editor with `o`
  - Yank change ID/revision to the system clipboard with `y`/`Y`
- Files
  - View files in current change and diff in side panel
  - See a change's files from the log tab with `Enter`
  - View conflicts list in current change
  - Toggle the diff between color words (default), git diff, and a `--stat` summary with `w`
  - Browse the whole repo at the shown revision in your editor with `o`
  - Mark files with `Space`/`a`, then mark the revisions touching any of them on the log tab
    with `T`
  - Untrack file with `x`
- Tags
  - View list of tags, including remote tags with `a`
  - Delete with `d`, track/untrack remote tags with `t`/`T`
  - Show the tagged revision on the log tab with `Enter`
- Bookmarks
  - View list of bookmarks, including from all remotes with `a`
  - Create with `c`, rename with `r`, delete with `d`, forget with `f`
  - Track bookmarks with `t`, untrack bookmarks with `T`
  - Create new change with `n`, edit change with `e`/`E`
  - Browse the whole repo at a bookmark's revision in your editor with `o`
  - Push a single bookmark with `p`
- Workspaces
  - See every jj workspace with the facts that decide whether it can go: directory present or
    missing, working copy empty or holding work, base immutable or not, when jj last ran there
  - Filter to cleanup candidates with `a`, cycle the sort (state, age, name) with `s`
  - Mark with `Space` (every listed candidate with `A`), then forget with `f`, or forget and
    delete the directory with `D`. Each workspace is snapshotted first, so forgetting never loses
    tracked work: an empty working copy is abandoned, anything else stays in the log as a commit
  - Bring a stale workspace up to date with `U`, undo with `u`
  - Show the working-copy commit on the log tab with `Enter`, open the directory in your editor
    with `o`
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
- `jjscope.diff-format`: Change the default diff format. Can be `color-words`, `git`, or `stat`.
  Defaults to `stat` for whole-revision panels (log, bookmarks, tags) and `color-words` for the
  files tab's per-file diff; setting it explicitly applies one format everywhere
  - If `jjscope.diff-format` is not set but `ui.diff.format` is, the latter will be used
- `jjscope.diff-tool`: Specify which diff tool to use by default
  - If `jjscope.diff-tool` is not set but `ui.diff.tool` is, the latter will be used
- `jjscope.bookmark-template`: Change the bookmark name template for generated bookmark names. Defaults to `'push-' ++ change_id.short()`
  - If `jjscope.bookmark-template` is not set but `templates.git_push_bookmark` is, the latter will be used
- `jjscope.layout`: Changes the layout of the main and details panel. Can be `horizontal` (default) or `vertical`
- `jjscope.layout-percent`: Changes the layout split of the main page. Should be number between 0 and 100. Defaults to `50`
- `jjscope.description-transforms`: Define keys that rewrite a change's description. See [Description transforms](#description-transforms)

Example: `jj config set --user jjscope.diff-format "color-words"` (for storing in [user config file](https://martinvonz.github.io/jj/latest/config/#user-config-file), repo config is also supported)

### Description transforms

A description transform rewrites the description of a change with a single
keystroke. Each transform gets its own key in the log tab:

```toml
[[jjscope.description-transforms]]
name = "archive"
key = "shift+g"
template = "archived: {{ desc }}"
description = "prefix the description with archived:"
```

`description` is what the help popup (`?`) shows for the key. It is optional and falls back
to `name` — the template is never shown, since a multi-line one would fill the popup with
Jinja.

`template` is a [Jinja](https://docs.rs/minijinja/latest/minijinja/syntax/index.html)
template rendered with the change's current description in scope as `desc`. The
rendered result is trimmed, so a template can be laid out over several lines
without adding blank lines to the description.

This pairs well with a revset that hides changes by description — for example,
if `archived: ` marks a head as no longer interesting, one keypress archives it:

```toml
[revsets]
log = "present(@) | ancestors(immutable_heads().., 2) | present(trunk()) | (visible_heads() ~ description(glob:'archived: *'))"
```

Because the template is a full Jinja template, a transform can inspect the
description it is rewriting. Making the archive key a toggle, so that pressing
it on an already-archived change un-archives it:

```toml
[[jjscope.description-transforms]]
name = "archive"
key = "shift+g"
description = "archive or un-archive this change"
template = """
{%- if desc is startingwith("archived: ") -%}
  {{ desc | removeprefix("archived: ") }}
{%- else -%}
  archived: {{ desc }}
{%- endif -%}
"""
```

Note that Jinja is not Python: strings have no `.startswith()` method. Prefixes
and suffixes are checked with the `startingwith` and `endingwith` *tests*
(`desc is startingwith(...)`), and string operations are *filters*
(`desc | upper`). Alongside MiniJinja's
[builtin filters](https://docs.rs/minijinja/latest/minijinja/filters/index.html),
jjscope provides `removeprefix` and `removesuffix`, which strip an affix only
when present — unlike `| replace`, which would also strip occurrences from the
middle of the description.

Transforms apply to the marked changes, or to the selected change if none are
marked, and take effect immediately — `u` undoes them. Nothing is written until
every change has passed the immutability check and every template has rendered,
so a batch either applies completely or not at all; a template error is reported
in a popup naming the transform. Transform keys are bound after all other
keybindings, so they override a built-in binding on the same key.

## Usage

To start jjscope for the repository in the current directory: `jjscope`

To use a different repository: `jjscope --path ~/path/to/repo`

To start with a different default revset: `jjscope -r '::@'`

## Key mappings

See all key mappings for the current tab with `?`.

### Basic navigation

- Quit with `q`
- Change tab with `1`/`2`/`3`/`4` or with `h`/`l`
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
- Browse the whole repo as it existed at the highlighted revision with `o`: the revision's
  file tree is extracted to a temporary directory and opened in your editor (`ui.editor`,
  else `$VISUAL`/`$EDITOR`), with the editor's working directory set to that tree so file
  pickers and `:grep` stay inside the revision. Opened read-only where the editor supports
  it, since the temp tree is deleted when the editor exits
  - Git LFS files are extracted as their pointer text rather than their contents, so opening
    a revision stays fast and never waits on an LFS download
- Search the visible log text with `/`, vim-style: type a query (matches highlight as you
  type), press `Enter` to jump to the first match, then `n`/`N` to step to the next/previous
  match (wrapping). `Esc` clears the search. Matching is case-insensitive and only covers
  what's shown in the log — a change present in the tree but filtered out of the view won't
  be found, which is expected. While a search is active `n`/`N` navigate matches instead of
  creating changes; once it's cleared they revert to new-change
- Display different revset with `r` (`jj log -r`)
- Set the log's revset and the revset to *mark* within it with `Ctrl+r`. The popup has two
  fields — `Show:` and `Mark:` — with `Tab` to switch between them, `Ctrl+s` to apply and `Esc`
  to cancel. Both start empty: an empty `Show:` means the configured default revset, and an
  empty `Mark:` marks nothing
  - Every revision the `Mark:` revset selects gets a colored gutter bar (`▌`) on both of its
    lines. Nothing else about the log changes — same revset, same graph, same node glyphs, same
    selection highlight — so the marks read *against* the surrounding history instead of
    replacing it. This is the difference from putting the expression in `Show:`, which filters
    the log and throws away the context that makes the answer useful
  - Any [revset](https://docs.jj-vcs.dev/latest/revsets/) works: `conflicts()`,
    `description(glob:'*wip*')`, `author('alice')`, `mine()`, `files('src/ui')`. The gutter
    composes with everything else, so a marked revision can be selected, `Space`-marked, and a
    `/` search hit all at once
  - The `Mark:` revset is evaluated across the whole repo, *not* intersected with `Show:`, which
    is what lets jjscope tell "42 revisions match, none in this revset" apart from "nothing
    matches". In the first case the panel title says so — `marking: justfile (0 of 3 in view —
    ctrl+w to show)` — and `Ctrl+w` widens `Show:` to the marking expression. The title keeps
    saying it for as long as it's true, since an empty gutter is exactly the thing you scroll
    around looking at, and the offer would be useless if scrolling dismissed it. Intersecting
    instead would just silently show nothing
  - Marks follow revisions through rewrites: they are keyed by change id, so squashing,
    rebasing, or describing a marked revision keeps its bar, and the set is recomputed on every
    refresh so moving hunks between revisions moves the bars with them
  - `Esc` clears the marking (when no `/` search is active, which `Esc` clears first).
    Session-only, like `/` search — there is no config key for it
- Mark the revisions touching a path with `T`, a shortcut for writing `files(...)` in the
  `Mark:` field: type a path or [fileset](https://docs.jj-vcs.dev/latest/filesets/) and press
  `Enter`. `T` again clears it, and the panel title shows the path
  - A bare path uses jj's default matching (`prefix-glob:`), so `src/ui` marks everything under
    that directory and `src/*.rs` honours the glob. Prefix it to change that: `glob:'src/**/*.rs'`,
    `file:src/app.rs` for one exact file, `root:src` to resolve from the workspace root instead
    of the working directory
- Mark the revisions *related* to the marked ones with `Ctrl+t`: takes the union of the files the
  marked revisions touch (or the selected revision's, if none are marked) and marks every revision
  touching any of them. `Ctrl+t` again clears it, and the title says how many revisions and files
  it drew from
  - The source revisions match themselves, since their own files are in the set — that's wanted,
    as it shows the group being compared against
  - For a specific subset of files rather than a whole revision's worth, mark the files on the
    files tab with `Space` and press `T` there instead; both land in the same highlight
- Change details panel diff format with `w`, cycling `--stat` (the default: just the files
  touched, with added/removed counts) → color words → Git → a diff tool if one is set
- Cursor onto an `(elided revisions)` row and press `Enter` to reveal what it stands for. jj
  prints that placeholder where the revset selects around revisions without containing them;
  the cursor now stops on it like any other row, and `Enter` widens the log's revset to fill in
  that one gap
  - Only that gap: with several branches partly shown, expanding one leaves the others elided.
    The added term is the range between the placeholder's revision and its nearest *shown*
    ancestors, so it is an ordinary revset — `Ctrl+r` afterwards shows and edits it like any other
  - The row is a placeholder, not a change, so commands that act on a revision refuse while the
    cursor is parked there rather than quietly acting on the revision above it. Navigation, the
    view controls, and `Enter` are what work
- Toggle details panel wrapping with `W`
- Create new change after highlighted change with `n` (`jj new`)
  - Create a new change without moving `@` into it with `N` (`jj new --no-edit`), for setting up
    a place to work without leaving the one you are in — the cursor moves to the new change, so
    `e` there enters it if you change your mind
- Splice rather than append by marking a *before*-anchor with `i` (`⌄` in the graph): the change
  `n` creates or `r` moves lands *below* it, i.e. the anchor becomes its child. `Space` marks
  *after*-anchors (`✓`), which become its parents. The two marks are exclusive on a revision
  - With no before-anchor, `n` and `r` behave exactly as they always have — a before-anchor is an
    extra key you press only when you want a splice, never a phase you have to step through
  - `n` with a before-anchor runs `jj new --no-edit -A -B`, so `@` stays where it is and the cursor
    is placed on the inserted change (press `e` there to edit into it). The confirmation says it is
    inserting rather than creating a leaf
  - `r` with a before-anchor runs `jj rebase -r -A -B` instead of rebasing onto a parent set. Set
    the before-anchor first, then press `r` on the change to move, then `Space` to pick what it
    goes after. The panel title switches to `Insert` so the change of mode is visible, and `i`
    toggles it back mid-gesture
  - In insert mode the current parents are *not* pre-seeded as marks: the after-anchors are an
    absolute set you pick, not an edit of today's parents
  - `Esc` clears both kinds of mark
- Create a new change *beside* the selected one, sharing its parents, by pressing `r` then `n`
  (or `N` to leave `@` where it is). The rebase gesture seeds the marks with the change's current parents,
  so `n` there builds a sibling instead of moving anything; the gesture ends and the usual `n`
  confirmation shows how many parents it picked up
- Git submodules are surfaced read-only, since jj ignores them entirely (it prints
  `ignoring git submodule at ...` on import and never interprets the gitlink again)
  - A revision that moves a submodule pointer shows what actually changed, instead of jj's
    `vendor/inner | 1 +` — which counts the 40-byte object id as a one-line text edit:

    ```
    Submodule vendor/inner:
        cc993a43 → f5ea02cf
          f5ea02c inner v4
          f6c2046 inner v3
    ```

    The commit list comes from the submodule's own checkout, so a submodule that isn't checked
    out (or whose objects were never fetched) shows the pointers alone and says so
  - The log panel title warns when a submodule's checked-out commit no longer matches what `@`
    records — `submodule vendor/inner moved, not recorded in @`. jj cannot see this at all:
    `jj status` reports a clean working copy while `git status` reports `M vendor/inner`, so
    without the warning the UI denies that an uncommitted change exists
  - Nothing here writes to a submodule. jj cannot track such a change, so jjscope would only be
    creating state that jj disagrees with — use `git` directly for that
  - Repos with no `.gitmodules` pay nothing: the check is a single filesystem test
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
- Set a tag on the highlighted change with `t` (`jj tag set`). The field starts empty, so
  typing a name always *creates* a tag — a revision can carry any number of them, and the
  ones already there are listed below the field for reference. Naming one that already exists
  is a move, which jjscope confirms first (`--allow-move`), since a tag is usually a release
  marker you do not want to relocate by accident. Move, rename, and delete existing tags on
  the [Tags tab](#tags-tab)
  - Scroll in bookmark list with `j`/`k`
  - Create a new bookmark with `c`
  - Use auto-generated name with `g`
- Squash changes with `s` (`jj squash --from --into`): press `s` to pick up the marked changes
  (or the highlighted one), then pick the destination and press `Enter`
  - The cursor starts on the parent, so `s` then `Enter` squashes into the parent (like bare `jj squash`)
  - Squash ignoring immutability with `S` (`jj squash --ignore-immutable`)
  - Press `s` again during the gesture to toggle interactive mode (`jj squash -i`): on `Enter`
    the configured diff editor opens to pick the hunks that move; the title shows which mode is active
- Split the highlighted change with `-` (`jj split -r`): the configured diff editor opens
  to pick the hunks for the first of the two resulting changes
- Edit the highlighted change's diff with `=` (`jj diffedit`): press `=` to pick up the
  change, then press `Enter` to edit its own diff (plain `jj diffedit -r`), or pick a base to
  edit it against first. The cursor stays on the change, so `=` then `Enter` edits the
  change's diff against *all* its parents — which for a merge is only what the merge itself
  changed. Marking a revision, or moving the cursor off the change, edits against that
  revision instead (`jj diffedit --from <base> --to <change>`), letting you drop or restore
  changes relative to any ancestor, not just the parent. The configured diff editor opens on
  the chosen diff; deselected hunks are dropped from the change and its descendants (undo
  with `u`)
  - On a merge, a base is not the same as no base: `--from` against one parent shows the
    *other* parent's changes as part of the diff, so deselecting them would revert that
    branch's work rather than drop this change's. Use `Enter` with the cursor left in place
    to edit just the merge's own contribution
  - An *empty* change can still be diff-edited against a different base: "empty" means empty
    against its own parents, and against an earlier ancestor there may well be a diff. So `=`
    starts the gesture on an empty change, and only refuses if you then ask for its own diff
    (`Enter` with the cursor left in place) — leaving the gesture up so you can pick a base
    instead
- Rebase changes with `r` (`jj rebase -s`/`-r`): press `r` to pick up the marked changes
  (or the highlighted one), then edit the parent set and press `Enter`. Descendants come
  along by default (`jj rebase -s`)
  - The picked-up change's current parents appear marked with `✚`; `Space` toggles any
    change in or out of the parent set, so parents can be added and removed in one go
    (e.g. adding/dropping branches from a megamerge)
  - If the parent set is left untouched, `Enter` rebases onto the highlighted change
    instead — the plain "move it there" gesture
  - Press `r` again during the gesture to switch to moving just that change
    (`jj rebase -r`), leaving its descendants behind on its old parents; press `r` again to
    switch back. The title shows which mode is active
- Rebase a whole branch with `B` (`jj rebase -b`): pick up a change on the branch, press
  `B`, then pick the destination(s) and press `Enter`
  - Which commits get new parents (the branch roots) depends on the destination, so
    there is no parent set to edit here — it's a plain destination pick
- Git fetch with `f` (`jj git fetch`)
  - Git fetch all remotes with `F` (`jj git fetch --all-remotes`)
- Git push with `p` (`jj git push`)

### Files tab

- Select current change with `@`
- Open the selected file in your editor (`ui.editor`, else `$VISUAL`/`$EDITOR`) with `Enter`
  - On `@` the live working-copy file is opened for editing
  - On any other revision the file's content at that revision is opened read-only
    (from `jj file show`), since that content isn't on disk to edit in place
- Browse the whole repo at the revision being shown with `o` (same as the log tab). The files
  list only holds the files that revision *changed*, so this is how to reach everything else
  at that revision
- Mark files with `Space` (`✓` in the leading column), or `a` to mark every file in the revision
  — pressing `a` again when all are marked clears them
  - Marks persist as you move between revisions, so a file set can be built up while browsing.
    The panel title keeps a count, since a mark on a file the current revision doesn't touch has
    no glyph to show
- Mark the revisions touching those files with `T`, the same key as on the log tab: switches to
  the log tab with every revision touching *any* of the marked files marked, so you can see what
  else has been near them. With nothing marked it uses the selected file, so `T` alone behaves as
  it always has. Matched exactly (`file:`), since the paths came from jj rather than from you —
  where a path typed on the log tab prefix-matches, so a directory works
  - A renamed file may hand over only the changed part of its path, since jj writes renames as
    `src/{old.rs => new.rs}`. Nothing matches in that case; retype the full path with `T` on the
    log tab
- Files jj refused to snapshot are listed after the revision's own files, marked `?`, with a
  count in the panel title. In practice these are files over `snapshot.max-new-file-size`:
  jj warns about them but they belong to no revision, so they appear in no diff. Selecting
  one shows why it was refused and how to resolve it, since there is no diff to show. Only
  shown for `@` — `jj status` always reports the working copy
  - `x` works on these too, adding the file to `.gitignore` so jj stops warning about it
- Resolve the selected file's conflict with `v`/`V` (`jj resolve --tool :theirs`/`:ours`)
  - `v` keeps the rebased/squashed revision's version; `V` keeps the rebase/squash destination's version
  - `m` resolves in the configured merge editor (`jj resolve`), file by file on the log tab
    or just the selected file on the files tab
- Diff format for the selected file with `w`, cycling color words (the default here) → Git →
  a diff tool if one is set → `--stat`. Unlike the whole-revision panels, this one shows a
  single file, so it opens on the diff rather than the one-line stat summary of it
- Toggle details panel wrapping with `W`

### Bookmarks tab

- Search bookmarks with `/`, identical to the log tab: type a query (matches highlight as you
  type), `Enter` jumps to the first match, `n`/`N` step to the next/previous match (wrapping),
  `Esc` clears. Case-insensitive; matches the displayed bookmark text (name and remote) and
  leaves non-matching bookmarks visible. While a search is active `n`/`N` navigate matches
  instead of creating changes
- Browse the whole repo at the selected bookmark's revision with `o` (same as the log tab)
- Show bookmarks with all remotes with `a` (`jj bookmark list --all`)
- Create a bookmark with `c` (`jj bookmark create`)
- Rename a bookmark with `r` (`jj bookmark rename`)
- Delete a bookmark with `d` (`jj bookmark delete`)
- Forget a bookmark with `f` (`jj bookmark forget`)
- Track a bookmark with `t` (only works for bookmarks with remotes) (`jj bookmark track`)
- Untrack a bookmark with `T` (only works for bookmarks with remotes) (`jj bookmark untrack`)
- Change details panel diff format with `w`, cycling `--stat` (the default: just the files
  touched, with added/removed counts) → color words → Git → a diff tool if one is set
- Toggle details panel wrapping with `W`
- Create a new change after the highlighted bookmark's change with `n` (`jj new`)
  - Create a new change and describe with `N` (`jj new -m`)
- Edit the highlighted bookmark's change with `e` (`jj edit`)
  - Edit the highlighted bookmark's change ignoring immutability with `E` (`jj edit --ignore-immutable`)
- Push the highlighted bookmark with `p` (`jj git push -b <bookmark>`)

### Tags tab

Tags gained first-class support in jj 0.44 (set, delete, and per-remote tracking). Tags are
*created* from the log tab with `t`, where you can see the revision being tagged; this tab is
for browsing and managing the ones that exist.

- Show the tagged revision on the log tab with `Enter`
- Show remote tags alongside local ones with `a` (`jj tag list --all-remotes`)
- Delete the selected local tag with `d` (`jj tag delete`), after a confirmation. The tagged
  revision itself is kept
- Move the selected tag to another revision with `m`: type any revset (`@`, a change id, a
  bookmark or tag name) and press `Enter` (`jj tag set --allow-move`)
- Rename the selected tag with `r`. jj has no rename, so this sets the new name at the same
  revision and deletes the old one; the new name is created first, so a failure leaves the
  original tag intact
- Track the selected remote tag with `t`, untrack it with `T` (`jj tag track`/`untrack`).
  Only applies to remote tags — select one with `a` first
- Change details panel diff format with `w`
- Refresh with `R`

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
