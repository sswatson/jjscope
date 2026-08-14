# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- next-header -->

## [Unreleased] - ReleaseDate

### Breaking Changes

- The details panel now opens in the new `--stat` format instead of color words, and `w` cycles
  stat → color words → git → diff tool (if set). The shape of a change is usually what you want
  first, and `w` is one keypress from the contents. Set `jjscope.diff-format = "color-words"` to
  get the old default back; an explicitly configured format, `ui.diff.format`, or a configured
  diff tool all still take precedence, so only users with no diff config see the change
- Log tab: the separate insert commands (`i`/`I`) are gone. Splicing is no longer its own verb:
  `i` now marks the selected revision as a *before*-anchor (`⌄`), the counterpart to `Space`'s
  *after*-anchor (`✓`), and `n`/`r` read both sets. `n` with a before-anchor inserts
  (`jj new --no-edit -A -B`) instead of creating a leaf; `r` with one inserts
  (`jj rebase -r -A -B`) instead of rebasing onto a parent set. Appending as a leaf is just the
  case where no before-anchor is set, which is what jj's own model says it is — and it stays a
  single keypress, with no before-phase to step past. The `insert-new` and `insert-move` config
  keys are removed; `i` is fixed, like `Space`
- Log tab: marking two changes where one is an ancestor of the other no longer force-infers a
  splice. That reading overrode a valid one — those two changes as the parents of a merge — which
  is now reachable. Say `i` to get the splice instead
- Log tab: rebase (`r`) now brings descendants along by default (`jj rebase -s`); pressing
  `r` again during the gesture switches to moving just that change (`jj rebase -r`). The
  two modes were previously the other way round. Moving a change usually means moving the
  work built on top of it, and the `-r` outcome — descendants re-parented onto the moved
  change's *old* parents — is the surprising one to get by default
- Files tab: `x` now adds the file to the repo-root `.gitignore` before untracking it, and
  asks for confirmation first. Previously it ran `jj file untrack` alone, which jj rejects
  unless the file is already ignored ("Files that are not ignored will be added back by the
  next command") — so the common case of "I meant to ignore this" failed with a popup telling
  you to go edit `.gitignore` by hand. The pattern is anchored to the repo root
  (`/sub/debug.log`) so it matches that one file, gitignore metacharacters in the path are
  escaped, and an already-present pattern is not duplicated. The file itself is kept on disk;
  the confirmation exists because `jj undo` restores the tracking but leaves the `.gitignore`
  line behind
- Log tab: `r` and `Ctrl+r` are swapped — rebase is now `r` (the more common operation
  gets the easier key) and the revset editor is `Ctrl+r`. Branch rebase (`jj rebase -b`)
  is a separate gesture on `B`
- Log tab: `Shift+p` (`jj git push --all`) is removed — too much of a footgun for a
  single keypress; use the CLI when pushing everything is really intended
- Edit (`e`/`E`, on the log and bookmarks tabs) no longer asks for confirmation:
  editing into a change is a frequent, cheap, and undoable action, and the dialog made
  it feel like a dangerous one. The immutability guard for plain `e` remains
- Log tab: squash (`s`/`S`), rebase (`r`), and insert (`i`/`I`) now share one
  "pick up, put down" gesture. The action key picks up the marked changes (or the
  highlighted one if none are marked); then pick the destination — or anchors, for
  insert — with the cursor (`Space` to mark several) and press `Enter` to execute
  (`Esc` cancels). Squash pre-places the cursor on the parent, so `s` `Enter` is
  `jj squash`; picking several destinations for rebase rebases onto their merge, and
  picking up several changes rebases/squashes them all. The separate squash and insert
  confirmation dialogs are gone — the final `Enter` is the confirmation
- Inserting a new change (`i`) with exactly two picked changes where one is an ancestor
  of the other skips the second phase and inserts between them immediately — the
  after/before assignment is inferred from the ancestry, since the reverse would be a cycle
- Inserting a new change (`i`) now passes `--no-edit`: `@` stays where it is instead of
  moving to the inserted change, so the printed graph keeps its shape and an empty
  undescribed `@` (e.g. a megamerge working set) isn't silently abandoned by `@` moving
  away. The cursor is placed on the inserted change instead — press `e` there to edit
  into it. (`n` still moves `@`, since starting new work there is its purpose)
- The keybinds config section is now kebab-cased: `[blazingjj.keybinds.log_tab]` must be
  changed to `[blazingjj.keybinds.log-tab]`
- Fork project and change name from "blazingjj" to "jjscope": the binary, crate, config
  table (`[blazingjj]` → `[jjscope]`), env vars (`BLAZINGJJ_LOG`/`BLAZINGJJ_TRACE` →
  `JJSCOPE_LOG`/`JJSCOPE_TRACE`), and log file (`blazingjj.log` → `jjscope.log`) are all renamed

### Fixed

- Log tab: a `jj new` that jj rejects — inserting before an immutable commit, say — now shows
  the error in a popup instead of exiting the TUI. The error propagated out of `update()`,
  which tears the whole app down; a refused command should just report itself
- Files tab: conflicted files are listed again for revisions whose conflicted paths are long.
  `jj resolve --list` pads the path column to the longest path but only up to a minimum width,
  so a lone long path is followed by a single space — and the parser required four, silently
  dropping every such conflict and showing the revision as having none. The same greedy pattern
  also captured the column padding as part of the path when it did match, so `v`/`V`/`m` on a
  short-named conflicted file reported "The selected file has no conflict to resolve". Paths are
  now split off the fixed `N-sided conflict...` description instead, which also keeps paths that
  contain spaces intact
- Browsing a revision with `o` no longer expands Git LFS pointers. `git archive` runs the
  smudge filter by default, so a revision whose tree is a few dozen MB could materialize
  many times that — in one repo a 135-byte pointer became a 1 GB file, making `o` take 3.7s
  and write 1.5 GB instead of 0.18s and 43 MB. Objects missing from the local LFS cache were
  fetched over the network, blocking the TUI for the length of the download. LFS files now
  extract as their pointer text

### Added

- Git submodules are now visible, read-only. jj ignores submodules — it carries the gitlink but
  never interprets it — so a pointer bump rendered as `vendor/inner | 1 +`, counting a 40-byte
  object id as a one-line text edit. The details panel now shows the old and new pointers and
  the commits between them, read from the submodule's own checkout; when the submodule isn't
  checked out it shows the pointers and says the commits are unavailable. Separately, the log
  panel title warns when a submodule's checked-out commit differs from what `@` records — jj's
  own status reports a clean working copy in that case, so the change was previously invisible.
  Nothing writes to submodules: jj could not track it. Repos without `.gitmodules` are
  unaffected, the check being a single filesystem test
- Details panel: added a `--stat` view — just the files a change touches with their
  added/removed counts — for taking in the shape of a change without reading it. jj sizes the
  histogram bars to the terminal width, so unlike the other text formats this one is cached per
  panel width and re-renders on resize
- Log tab: `r` then `n` (or `N`) creates a new change beside the selected one, sharing its
  parents. The rebase gesture already seeds the marks with the change's current parents, and
  every command reads the same marks, so `n` there builds a sibling rather than moving
  anything — the operation falls out of the mark model rather than needing a key of its own.
  The gesture ends cleanly instead of leaving a phase whose marks another command consumed
- Log tab: mark the revisions matching a revset with a gutter bar (`▌`), leaving the log
  otherwise untouched — same revset, same graph, same node glyphs — so the marks read against
  the surrounding history instead of replacing it. `Ctrl+r` now edits two fields, `Show:` (the
  log's revset) and `Mark:` (the revset to mark within it), with `Tab` to switch between them;
  both start empty. Any revset works, so `conflicts()`, `description(glob:'*wip*')`, and
  `files('src/ui')` all mark in place rather than filtering the view. The `Mark:` revset is
  evaluated repo-wide rather than intersected with `Show:`, which lets jjscope tell "3
  revisions match, none in this revset" apart from "nothing matches"; in that case the panel
  title reads `marking: justfile (0 of 3 in view — ctrl+w to show)` and keeps saying so while it
  holds, with `Ctrl+w` widening `Show:` to the marking expression. Marks are keyed by change id,
  so they follow revisions through squash, rebase, and describe, and are recomputed on each
  refresh so moving hunks moves the marks
- Log tab: mark the revisions touching a path with `T`, a shortcut for writing `files(...)` in
  the `Mark:` field. A bare path uses jj's default `prefix-glob:` matching, so a directory marks
  everything beneath it; `glob:`, `file:`, and `root:` prefixes are passed through to jj
- Files tab: mark the revisions touching the selected file with `T` — the same key as on the log
  tab — switching to the log tab with that file marked, so neither entry point needs the path
  typed or pasted. Matched exactly, since the path came from jj rather than from the user
- Tags tab: move a tag to another revision with `m` and rename one with `r` (jj has no
  `tag rename`, so this sets the new name and deletes the old). Both prompt for input and
  report jj's own error in place if the revset or name is rejected
- Tag support, using the first-class tag commands jj gained in 0.44. Set a tag on the
  highlighted change from the log tab with `t` (`jj tag set`); moving an existing tag is
  confirmed first, since jj requires `--allow-move` and a tag is usually a release marker.
  A new Tags tab (`4`) lists tags with the revision each points at, shows remote tags with
  `a`, deletes a local tag with `d` (the revision is kept), tracks and untracks remote tags
  with `t`/`T` (`jj tag track`/`untrack`), and jumps to the tagged revision on the log tab
  with `Enter`. Requires jj 0.44 or newer for the tab; the log tab's `t` needs `jj tag set`.
  The set-tag field starts empty so that typing always creates a tag — a revision can carry
  several — with the ones already present listed underneath for reference
- Files tab: show working-copy files jj refused to snapshot, listed with `?` after the
  revision's own files and counted in the panel title. These are typically files over
  `snapshot.max-new-file-size`; jj prints a warning about them but they belong to no revision,
  so `jj diff` never mentions them and the tab previously gave no sign they existed.
  Selecting one shows why it was refused — its size against the configured limit — and the
  two ways out, since there is no diff to display. Only shown for `@`, as `jj status` always
  describes the working copy. `x` works on them too, adding the file to `.gitignore` so jj
  stops warning
- Description transforms take an optional `description`, shown for the key in the help popup.
  It falls back to `name`; the Jinja template is no longer shown there, since a multi-line
  one filled the popup with `{%- if ... -%}` markup
- Configurable description transforms: `jjscope.description-transforms` defines keys that
  rewrite a change's description in one keystroke. Each entry declares its own `key` and a
  Jinja `template` rendered with the current description in scope as `desc`, so a transform
  can inspect what it is rewriting — making the archive key a toggle, for instance, by
  stripping the prefix when it is already there. Beyond MiniJinja's builtins, `removeprefix`
  and `removesuffix` filters strip an affix only when present. Transform keys are bound after
  every other keybinding, so they can override a built-in one. They apply to the marked
  changes, or the selected change if none are marked, and take effect immediately (`u` undoes
  them); nothing is written unless every change is mutable and every template renders, so a
  batch applies completely or not at all. Useful with a revset that hides changes by
  description, e.g. archiving a head by prefixing `archived: `
- Browse the whole repo as it existed at a revision with `o`, on the log, files, and bookmarks
  tabs. The revision's file tree is extracted to a temporary directory (via `git archive`
  against jj's git object store) and opened in the configured editor, with the editor's
  working directory set to the tree so file pickers, `:grep`, and relative paths stay inside
  the revision. The tree opens read-only where the editor supports it (`-R` for the vi family)
  and is deleted when the editor exits. On the files tab this reaches files the revision did
  not change, which the files list — a diff summary — does not include. Works from secondary
  workspaces and in non-colocated repos, since the object store is located by following jj's
  own `.jj/repo` and `store/git_target` pointers. Configurable as `open-tree` under
  `[jjscope.keybinds.log-tab]`
- Log tab: vim-style search with `/`. Type a query — matches highlight live as you type —
  then `Enter` jumps to the first match and `n`/`N` step to the next/previous match,
  wrapping around; `Esc` clears the search. Matching is case-insensitive and covers only
  the text shown in the log view (descriptions, IDs, bookmarks, authors, dates), so a
  change filtered out of the current revset is simply not found. While a search is active
  `n`/`N` navigate matches instead of creating changes; clearing the search reverts them.
  Configurable as `search` under `[jjscope.keybinds.log-tab]`
- Bookmarks tab: `/` is now the same vim-style search as the log tab (highlight as you type,
  `Enter` to jump, `n`/`N` to navigate, `Esc` to clear; matches name and remote) rather than
  a filter that hid non-matching bookmarks. Non-matching bookmarks stay visible
- Log tab: interactive squash and split, by handing the terminal to the user's
  configured diff editor (`ui.diff-editor`). Pressing `s` again during the squash
  gesture toggles interactive mode (`jj squash -i`), so `Enter` opens the diff editor
  to pick the hunks that move; `-` splits the highlighted change in two (`jj split -r`),
  picking the first half's hunks in the diff editor. The TUI suspends while the editor
  runs and refreshes in place when it returns
- Log tab: diff-edit a change against a chosen base with `=` (`jj diffedit`). `=` picks up
  the change and starts a pick gesture with the cursor left on it, so `=` then `Enter` edits
  the change's own diff against all its parents (`jj diffedit -r`). Marking a revision, or
  moving the cursor off the change, edits it relative to that revision instead
  (`jj diffedit --from <base> --to <change>`), so hunks can be dropped or restored against
  any ancestor rather than only the parent. Deselected hunks are dropped from the change and
  its descendants.
  For a merge these are genuinely different operations: `-r` shows only what the merge itself
  changed, while `--from` against a single parent also shows the other parent's changes, where
  deselecting a hunk would revert that branch's work. Picking two or more bases is rejected,
  since jj's `--from` resolves to exactly one revision
- Files tab: open the selected file in your editor with `Enter` (`ui.editor`, else
  `$VISUAL`/`$EDITOR`, else `vi`). On `@` the live working-copy file is opened for
  editing; on any other revision the file's content at that revision is materialized
  from `jj file show` into a temp file and opened read-only (`-R` for vi-family
  editors), since that content isn't on disk to edit in place. The TUI suspends while
  the editor runs and refreshes in place when it returns
- Resolve conflicts in the configured merge editor (`ui.merge-editor`) with `m`
  (`jj resolve -r`): on the log tab jj walks every conflicted file in the highlighted
  change; on the files tab only the selected file is resolved. Complements `v`/`V`,
  which keep one side wholesale without an editor
- Log tab: rebase (`r`) now edits the parent set in place: the picked-up change's
  current parents appear marked with `✚`, and `Space` toggles any change in or out of
  the candidate parent set — adding and removing parents (e.g. megamerge branches) in a
  single gesture. `Enter` applies the edited set; if the set was left untouched, `Enter`
  rebases onto the highlighted change instead (the plain "move it there" gesture).
  Pressing `r` again mid-gesture toggles whether descendants come along (`-s` vs `-r`);
  the rebase mode popup is gone. Branch rebase (`jj rebase -b`) is its own gesture on
  `B`: pick up a change on the branch, then pick the destination(s) — no parent set can
  be shown there, since which commits get new parents depends on the destination.
  `-A`/`-B` rebases are covered by insert-move (`I`)
- Log tab: simplify parents (remove redundant parent edges) of the marked/selected
  change(s) with `x` (`jj simplify-parents -r`), or of the change(s) and all their
  descendants with `X` (`-s`)
- Log tab: resolve all conflicts in the selected change with `v`/`V`
  (`jj resolve --tool :theirs`/`:ours`); files tab: same per-file. `v` keeps the
  rebased/squashed revision's version of each conflicted file, `V` keeps the
  rebase/squash destination's version
- Keybinding for jj absorb (`A`). After absorbing, the log temporarily marks
  the revisions that received hunks with `★` and the revisions that were only
  rebased along (including on sibling branches) with `☆`
- Top-level scroll keybindings (`scroll-down`, `scroll-up`, `scroll-down-half`,
  `scroll-up-half` under `[blazingjj.keybinds]`) that apply as defaults to all
  scroll-capable components and can be overridden per-component
- Message popup now supports scrolling with a scrollbar
- Command popup output now preserves ANSI color
- Drag to resize pane divider in all tabs
- Bookmarks tab: push a single bookmark by name with `p` (`jj git push -b`)
- Log tab: generate a new change id for the selected change with `c`/`C`
  (`jj metaedit --update-change-id`), useful for resolving divergence
- Log tab: insert a new change (`i`) or move the selected change (`I`) between marked changes,
  supporting combined `-A`/`-B` insert-after/insert-before anchors for `jj new`/`jj rebase`

### Changed

- Pressing `s` on the working copy now offers to squash into the parent (when there is exactly one)

### Fixed

- Log tab: `Shift+r` (refresh) now re-resolves the selected change instead of only
  re-fetching the graph. When jj activity outside the app rewrote or abandoned the
  selected change, the details panel — and every subsequent action — kept operating on
  the old commit, making the app look permanently stale until restarted. Refresh now
  follows the change's evolution and falls back to `@` if the change is gone
- Describing a commit with a message starting with a dash no longer fails
- Git push no longer passes `--allow-new`, which was removed in jj 0.42 and made every
  "push with new bookmarks" keybinding (`Ctrl+p`/`Ctrl+Shift+p`) fail, so those keybindings
  were merged into the regular push keybindings (`p`/`Shift+p`)
- Log tab: pressing `p`/`Shift+p` on a revision whose only bookmark(s) are brand new
  (never pushed/tracked) silently did nothing, since `jj git push -r <commit>` refuses
  to create new remote bookmarks and exits 0 with just a warning; the log tab now
  resolves bookmarks on the target revision and pushes them by name (`-b`), matching
  what the bookmarks tab already did, falling back to `-r <commit>` for bookmark-less
  revisions

## [0.8.0] - 2026-04-19

### Added

- Keybinding for jj duplicate
- Log panel can mark and abandon multiple commits
- Log panel create new revision with marked commits as parents
- Add support for copying the Change ID/revision of the current log tab entry using y/Y
- Fix Describe dialog width at git recommendation for commit message
- Log tab diff is cached
- Process multiple events per frame
- Go to top and bottom of visible log

### Fixed

- prevent (macos) os error 22 crash by capping event poll timeout

## [0.7.1] - 2026-01-16

### Fixed

 - Avoid unnecessary redraws on mouse move events which caused massive CPU spikes


## [0.7.0] - 2026-01-13

### Added

- Details panel responds to mouse scroll in all tabs
- Details panel sets COLUMNS to allow jj diff tool to fit window
- Update the details panel when gaining focus
- Added an animated popup for fetch/push operations

### Changed

- Move from bookmark-prefix to bookmark-template for the bookmark generation to match the behaviour from jj 0.31+
- Fork project and change name from "lazyjj" to "blazingjj"

### Removed

- The Command log tab

<!-- next-url -->
[Unreleased]: https://github.com/sswatson/jjscope/compare/v0.8.0...HEAD
[0.8.0]: https://github.com/blazingjj/blazingjj/compare/v0.7.1...v0.8.0
[0.7.1]: https://github.com/blazingjj/blazingjj/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/blazingjj/blazingjj/compare/v0.6.1...v0.7.0
