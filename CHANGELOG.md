# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- next-header -->

## [Unreleased] - ReleaseDate

### Breaking Changes

- Log tab: squash (`s`/`S`) and rebase (`Ctrl+r`) now operate on the *selected* change,
  like every other command, instead of moving `@`. Squash sends the selected change into
  its parent, or into the marked change if one is marked. Rebase moves the selected change
  onto the marked change(s) — mark the destination(s) with `Space` first; multiple marks
  rebase onto their merge. The old "squash @ into the selected change" is now: mark the
  destination with `Space`, jump to @ with `@`, then `s`
- The keybinds config section is now kebab-cased: `[blazingjj.keybinds.log_tab]` must be
  changed to `[blazingjj.keybinds.log-tab]`
- Fork project and change name from "blazingjj" to "jjscope": the binary, crate, config
  table (`[blazingjj]` → `[jjscope]`), env vars (`BLAZINGJJ_LOG`/`BLAZINGJJ_TRACE` →
  `JJSCOPE_LOG`/`JJSCOPE_TRACE`), and log file (`blazingjj.log` → `jjscope.log`) are all renamed

### Added

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
