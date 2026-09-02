#![expect(clippy::borrow_interior_mutable_const)]

//! Workspaces tab. Lists the repo's jj workspaces with the facts that decide
//! whether each is safe to clean up, and offers the cleanup actions: forget,
//! forget and delete the directory, and `update-stale`.
//!
//! The tab shows facts and asks before every action; it never decides for
//! the user. See [crate::commander::workspaces] for what the facts are and
//! why forgetting is safe.

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use ansi_to_tui::IntoText;
use anyhow::Result;
use ratatui::crossterm::event::Event;
use ratatui::crossterm::event::KeyCode;
use ratatui::crossterm::event::KeyEventKind;
use ratatui::prelude::*;
use ratatui::widgets::*;
use tracing::instrument;
use tui_confirm_dialog::ButtonLabel;
use tui_confirm_dialog::ConfirmDialog;
use tui_confirm_dialog::ConfirmDialogState;
use tui_confirm_dialog::Listener;

use crate::commander::CommandError;
use crate::commander::EditorCommand;
use crate::commander::new_commander;
use crate::commander::workspaces::Workspace;
use crate::commander::workspaces::WorkspaceState;
use crate::commander::workspaces::recover_roots;
use crate::env::DiffFormat;
use crate::env::JjConfig;
use crate::env::get_env;
use crate::ui::AppAction;
use crate::ui::Component;
use crate::ui::ComponentInputResult;
use crate::ui::dialog::HelpPopup;
use crate::ui::dialog::MessagePopup;
use crate::ui::panel::DetailsPanel;
use crate::ui::panel::TextContent;
use crate::ui::utils::PaneDivider;
use crate::ui::utils::tabs_to_spaces;

const FORGET_POPUP_ID: u16 = 1;
const REMOVE_POPUP_ID: u16 = 2;

/// Glyph in the leading column of a workspace marked for a bulk action. The
/// same glyph the log and files tabs use for their marks.
const MARK: &str = "✓";

/// How many workspaces a confirmation dialog lists by name before summarizing
/// the rest, so a large bulk action does not overflow the screen.
const CONFIRM_LIST_LIMIT: usize = 12;

/// Order of the workspace list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortMode {
    /// Most disposable first: ghosts, then idle workspaces (those on immutable
    /// history first), then ones holding work; oldest first within a group;
    /// the current workspace last.
    State,
    /// Least recently touched first; the current workspace last.
    Age,
    Name,
}

impl SortMode {
    fn next(self) -> Self {
        match self {
            SortMode::State => SortMode::Age,
            SortMode::Age => SortMode::Name,
            SortMode::Name => SortMode::State,
        }
    }

    fn label(self) -> &'static str {
        match self {
            SortMode::State => "by state",
            SortMode::Age => "by age",
            SortMode::Name => "by name",
        }
    }
}

/// Sort `workspaces` in place according to `mode`.
pub fn sort_workspaces(workspaces: &mut [Workspace], mode: SortMode) {
    match mode {
        SortMode::State => workspaces.sort_by(|a, b| {
            let key = |ws: &Workspace| {
                (
                    ws.current,
                    ws.state(),
                    !ws.on_immutable_base(),
                    ws.activity_timestamp(),
                )
            };
            key(a).cmp(&key(b)).then_with(|| a.name.cmp(&b.name))
        }),
        SortMode::Age => workspaces.sort_by(|a, b| {
            let key = |ws: &Workspace| (ws.current, ws.activity_timestamp());
            key(a).cmp(&key(b)).then_with(|| a.name.cmp(&b.name))
        }),
        SortMode::Name => workspaces.sort_by(|a, b| a.name.cmp(&b.name)),
    }
}

/// Render an age in seconds the way people say it: "3 days ago".
pub fn humanize_age(seconds: i64) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const WEEK: i64 = 7 * DAY;
    const MONTH: i64 = 30 * DAY;
    const YEAR: i64 = 365 * DAY;

    let seconds = seconds.max(0);
    let plural = |n: i64, unit: &str| {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    };
    if seconds < MINUTE {
        "just now".to_owned()
    } else if seconds < HOUR {
        plural(seconds / MINUTE, "minute")
    } else if seconds < DAY {
        plural(seconds / HOUR, "hour")
    } else if seconds < 2 * WEEK {
        plural(seconds / DAY, "day")
    } else if seconds < 2 * MONTH {
        plural(seconds / WEEK, "week")
    } else if seconds < 2 * YEAR {
        plural(seconds / MONTH, "month")
    } else {
        plural(seconds / YEAR, "year")
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn format_time(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

/// Shorten a path for display by replacing the home directory with `~`.
fn abbreviate_home(path: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME")
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() {
            "~".to_owned()
        } else {
            format!("~/{}", rest.display())
        };
    }
    path.display().to_string()
}

/// One-line account of what the working copy sits on.
fn base_label(ws: &Workspace) -> String {
    match (ws.parents, ws.mutable_parents) {
        (0, _) => "no parents".to_owned(),
        (1, 0) => "on an immutable parent".to_owned(),
        (1, _) => "on a mutable parent".to_owned(),
        (parents, 0) => format!("merge of {parents} immutable parents"),
        (parents, mutable) => format!("merge of {parents} parents ({mutable} mutable)"),
    }
}

/// The age shown in a list row. A ghost has no directory to date, so its
/// commit's rewrite time stands in, flagged as approximate.
fn age_label(ws: &Workspace, now: i64) -> String {
    match ws.last_touched {
        Some(touched) => humanize_age(now - touched),
        None => format!("~{}", humanize_age(now - ws.committer_timestamp)),
    }
}

fn badge(ws: &Workspace) -> Span<'static> {
    if ws.current {
        return Span::styled("current", Style::new().cyan().bold());
    }
    let state = ws.state();
    let style = match state {
        WorkspaceState::Ghost => Style::new().red().bold(),
        WorkspaceState::Idle => Style::new().green().bold(),
        WorkspaceState::HoldingWork => Style::new().yellow().bold(),
    };
    Span::styled(state.label(), style)
}

/// The two-line list row for a workspace.
fn workspace_item(ws: &Workspace, marked: bool, now: i64) -> ListItem<'static> {
    let mark = if marked {
        Span::styled(format!("{MARK} "), Style::new().cyan())
    } else {
        Span::raw("  ")
    };
    let first = Line::from(vec![
        mark,
        Span::styled(ws.name.clone(), Style::new().bold()),
        Span::raw("  "),
        badge(ws),
        Span::raw("  "),
        Span::styled(age_label(ws, now), Style::new().dark_gray()),
    ]);

    let mut second = vec![
        Span::raw("    "),
        Span::styled(ws.short_change_id().to_owned(), Style::new().magenta()),
        Span::raw(" "),
        Span::styled(ws.short_commit_id().to_owned(), Style::new().blue()),
        Span::raw(" "),
    ];
    if ws.empty {
        second.push(Span::styled("(empty) ", Style::new().green()));
    }
    if ws.described {
        second.push(Span::raw(ws.description.clone()));
    } else {
        second.push(Span::styled("(no description set)", Style::new().yellow()));
    }
    second.push(Span::styled(
        format!("  · {}", base_label(ws)),
        Style::new().dark_gray(),
    ));
    match ws.root.as_deref() {
        Some(root) => second.push(Span::styled(
            format!("  · {}", abbreviate_home(root)),
            Style::new().dark_gray(),
        )),
        None => second.push(Span::styled("  · no directory", Style::new().red())),
    }

    ListItem::new(Text::from(vec![first, Line::from(second)]))
}

/// The facts block at the top of the details panel.
fn details_header(ws: &Workspace, now: i64) -> Vec<Line<'static>> {
    let label = |text: &str| Span::styled(format!("{text:<14}"), Style::new().dark_gray());
    let mut lines = vec![Line::from(vec![
        Span::styled(ws.name.clone(), Style::new().bold()),
        Span::raw("  "),
        badge(ws),
    ])];

    match ws.root.as_deref() {
        Some(root) => {
            lines.push(Line::from(vec![
                label("Directory"),
                Span::raw(root.display().to_string()),
            ]));
            if ws.root_remembered {
                lines.push(Line::from(vec![
                    label(""),
                    Span::styled(
                        "remembered from an earlier listing: jj lost the record (an undo does that); the directory's own state confirms the name",
                        Style::new().dark_gray(),
                    ),
                ]));
            }
            if ws.hosts_repo {
                lines.push(Line::from(vec![
                    label(""),
                    Span::styled("holds the repo store (main workspace)", Style::new().cyan()),
                ]));
            }
        }
        None => lines.push(Line::from(vec![
            label("Directory"),
            Span::styled(
                "missing: deleted, or created before jj recorded workspace paths (0.38)",
                Style::new().red(),
            ),
        ])),
    }

    match ws.last_touched {
        Some(touched) => lines.push(Line::from(vec![
            label("Last touched"),
            Span::raw(format!(
                "{} ({})",
                humanize_age(now - touched),
                format_time(touched)
            )),
            Span::styled(
                "  from the directory's own jj state",
                Style::new().dark_gray(),
            ),
        ])),
        None => lines.push(Line::from(vec![
            label("Last touched"),
            Span::raw("unknown (no directory); commit last rewritten "),
            Span::raw(humanize_age(now - ws.committer_timestamp)),
        ])),
    }
    lines.push(Line::from(vec![
        label("Created"),
        Span::raw(format!(
            "{} ({})",
            humanize_age(now - ws.author_timestamp),
            format_time(ws.author_timestamp)
        )),
    ]));

    let mut working_copy = vec![
        label("Working copy"),
        Span::styled(ws.short_change_id().to_owned(), Style::new().magenta()),
        Span::raw(" "),
        Span::styled(ws.short_commit_id().to_owned(), Style::new().blue()),
        Span::raw(" "),
    ];
    if ws.empty {
        working_copy.push(Span::styled("(empty) ", Style::new().green()));
    }
    if ws.described {
        working_copy.push(Span::raw(ws.description.clone()));
    } else {
        working_copy.push(Span::styled("(no description set)", Style::new().yellow()));
    }
    lines.push(Line::from(working_copy));
    lines.push(Line::from(vec![label("Base"), Span::raw(base_label(ws))]));

    let forget = match ws.protected_reason() {
        Some(reason) => Span::styled(format!("protected: {reason}"), Style::new().cyan()),
        None => Span::raw(ws.forget_effect()),
    };
    lines.push(Line::from(vec![label("Forget"), forget]));
    lines.push(Line::from(""));
    lines
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CleanupKind {
    /// `jj workspace forget`, leaving directories on disk.
    Forget,
    /// Forget, then delete each workspace's directory.
    Remove,
}

impl CleanupKind {
    fn title(self) -> &'static str {
        match self {
            CleanupKind::Forget => "Forget workspace",
            CleanupKind::Remove => "Remove workspace",
        }
    }

    fn popup_id(self) -> u16 {
        match self {
            CleanupKind::Forget => FORGET_POPUP_ID,
            CleanupKind::Remove => REMOVE_POPUP_ID,
        }
    }
}

/// A cleanup the user has been asked to confirm.
struct PendingCleanup {
    kind: CleanupKind,
    /// Names captured when the dialog opened, so a refresh cannot change what
    /// is being confirmed.
    names: Vec<String>,
}

pub struct WorkspacesTab {
    workspaces_output: Result<Vec<Workspace>, CommandError>,
    list_state: ListState,
    list_height: u16,
    /// Timestamp the listing was taken at, so every row's age is relative to
    /// the same moment.
    now: i64,

    /// Show only ghosts and idle workspaces.
    candidates_only: bool,
    sort: SortMode,
    /// Workspaces marked with `Space` for a bulk action, by name.
    marked: BTreeSet<String>,
    /// Every root jj has reported this session, by workspace name. jj drops a
    /// workspace's root for good when the workspace is forgotten, so after an
    /// undo the workspace would otherwise come back as a ghost with its
    /// directory still on disk; see [recover_roots].
    known_roots: HashMap<String, PathBuf>,
    /// Name of the selected workspace. Tracked by name rather than index so a
    /// refresh, a re-sort, or the filter cannot silently move the selection.
    selected: Option<String>,

    panel: DetailsPanel,
    show_output: Option<Result<String, CommandError>>,

    pending: Option<PendingCleanup>,
    popup: ConfirmDialogState,
    popup_tx: std::sync::mpsc::Sender<Listener>,
    popup_rx: std::sync::mpsc::Receiver<Listener>,

    diff_format: DiffFormat,
    config: JjConfig,
    pane_divider: PaneDivider,
}

fn status(message: impl Into<String>) -> AppAction {
    AppAction::SetStatusMessage(message.into())
}

fn popup(title: &'static str, message: impl Into<String>) -> AppAction {
    AppAction::SetPopup(Some(Box::new(MessagePopup::new(title, message))))
}

fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

impl WorkspacesTab {
    #[instrument(level = "info", name = "Initializing workspaces tab", parent = None, skip())]
    pub fn new() -> Result<Self> {
        let diff_format = get_env().jj_config.diff_format();
        let (popup_tx, popup_rx) = std::sync::mpsc::channel();
        let config = get_env().jj_config.clone();
        let pane_divider = PaneDivider::new(config.layout_percent());

        let mut tab = Self {
            workspaces_output: Ok(Vec::new()),
            list_state: ListState::default(),
            list_height: 0,
            now: now_secs(),
            candidates_only: false,
            sort: SortMode::State,
            marked: BTreeSet::new(),
            known_roots: HashMap::new(),
            selected: None,
            panel: DetailsPanel::new(),
            show_output: None,
            pending: None,
            popup: ConfirmDialogState::default(),
            popup_tx,
            popup_rx,
            diff_format,
            config,
            pane_divider,
        };
        tab.refresh_workspaces();
        tab.refresh_details();
        Ok(tab)
    }

    fn all_workspaces(&self) -> Vec<Workspace> {
        self.workspaces_output.as_ref().cloned().unwrap_or_default()
    }

    /// The workspaces the list shows, filtered and sorted.
    fn visible(&self) -> Vec<Workspace> {
        let mut workspaces: Vec<Workspace> = self
            .all_workspaces()
            .into_iter()
            .filter(|ws| !self.candidates_only || ws.is_candidate())
            .collect();
        sort_workspaces(&mut workspaces, self.sort);
        workspaces
    }

    fn selected_index(&self, visible: &[Workspace]) -> Option<usize> {
        let selected = self.selected.as_deref()?;
        visible.iter().position(|ws| ws.name == selected)
    }

    fn selected_workspace(&self) -> Option<Workspace> {
        let selected = self.selected.as_deref()?;
        self.all_workspaces()
            .into_iter()
            .find(|ws| ws.name == selected)
    }

    /// Keep the selection on a listed workspace, falling back to the first.
    fn ensure_selection(&mut self) {
        let visible = self.visible();
        if self.selected_index(&visible).is_none() {
            self.selected = visible.first().map(|ws| ws.name.clone());
        }
    }

    pub fn refresh_workspaces(&mut self) {
        self.workspaces_output = new_commander().get_workspaces().map(|mut workspaces| {
            for ws in &workspaces {
                if let Some(root) = ws.root.clone()
                    && !ws.root_remembered
                {
                    self.known_roots.insert(ws.name.clone(), root);
                }
            }
            recover_roots(&mut workspaces, &self.known_roots);
            workspaces
        });
        self.now = now_secs();
        // Marks on workspaces that no longer exist would silently widen the
        // next bulk action's target list if they ever came back.
        let names: BTreeSet<String> = self
            .all_workspaces()
            .into_iter()
            .map(|ws| ws.name)
            .collect();
        self.marked.retain(|name| names.contains(name));
        self.ensure_selection();
    }

    pub fn refresh_details(&mut self) {
        let mut commander = new_commander();
        commander.limit_width(self.panel.columns() as usize);
        self.show_output = self.selected_workspace().map(|ws| {
            commander
                .get_workspace_show(&ws, &self.diff_format)
                .map(|show| tabs_to_spaces(&show))
        });
        self.panel.scroll_to(0);
    }

    fn refresh_all(&mut self) {
        self.refresh_workspaces();
        self.refresh_details();
    }

    fn scroll(&mut self, delta: isize) {
        let visible = self.visible();
        if visible.is_empty() {
            return;
        }
        let index = match self.selected_index(&visible) {
            Some(index) => index
                .saturating_add_signed(delta)
                .min(visible.len().saturating_sub(1)),
            None => 0,
        };
        self.selected = visible.get(index).map(|ws| ws.name.clone());
        self.refresh_details();
    }

    fn toggle_mark(&mut self) {
        let Some(name) = self.selected.clone() else {
            return;
        };
        if !self.marked.remove(&name) {
            self.marked.insert(name);
        }
    }

    /// Mark every listed cleanup candidate, or clear those marks if they are
    /// all set already.
    fn toggle_candidate_marks(&mut self) {
        let candidates: Vec<String> = self
            .visible()
            .into_iter()
            .filter(Workspace::is_candidate)
            .map(|ws| ws.name)
            .collect();
        if candidates.is_empty() {
            return;
        }
        if candidates.iter().all(|name| self.marked.contains(name)) {
            for name in &candidates {
                self.marked.remove(name);
            }
        } else {
            self.marked.extend(candidates);
        }
    }

    /// The workspaces a cleanup acts on: the marked ones, or the selected one
    /// when nothing is marked.
    fn cleanup_targets(&self) -> Vec<Workspace> {
        let all = self.all_workspaces();
        if self.marked.is_empty() {
            return self.selected_workspace().into_iter().collect();
        }
        all.into_iter()
            .filter(|ws| self.marked.contains(&ws.name))
            .collect()
    }

    fn confirm_cleanup(&mut self, kind: CleanupKind) -> ComponentInputResult {
        let targets = self.cleanup_targets();
        let (allowed, protected): (Vec<Workspace>, Vec<Workspace>) = targets
            .into_iter()
            .partition(|ws| ws.protected_reason().is_none());

        if allowed.is_empty() {
            let message = match protected.first() {
                Some(ws) => format!(
                    "Workspace {} cannot be cleaned up: {}.",
                    ws.name,
                    ws.protected_reason().unwrap_or_default()
                ),
                None => "No workspace selected.".to_owned(),
            };
            return ComponentInputResult::HandledAction(popup(kind.title(), message));
        }

        let count = allowed.len();
        let mut lines = vec![Line::from(match kind {
            CleanupKind::Forget => format!("Forget {}?", plural(count, "workspace", "workspaces")),
            CleanupKind::Remove => format!(
                "Forget {} and delete {} from disk?",
                plural(count, "workspace", "workspaces"),
                if count == 1 {
                    "its directory"
                } else {
                    "their directories"
                }
            ),
        })];
        lines.push(Line::from(""));
        for ws in allowed.iter().take(CONFIRM_LIST_LIMIT) {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(ws.name.clone(), Style::new().bold()),
                Span::raw(format!(": {}", ws.forget_effect())),
            ]));
            if kind == CleanupKind::Remove
                && let Some(root) = ws.root.as_deref()
            {
                lines.push(Line::from(Span::styled(
                    format!("      deletes {}", abbreviate_home(root)),
                    Style::new().red(),
                )));
            }
        }
        if count > CONFIRM_LIST_LIMIT {
            lines.push(Line::from(format!(
                "  ...and {} more",
                count - CONFIRM_LIST_LIMIT
            )));
        }
        lines.push(Line::from(""));
        match kind {
            CleanupKind::Forget => {
                lines.push(Line::from(
                    "Each workspace is snapshotted first, so unsnapshotted edits end up in its commit.",
                ));
                lines.push(Line::from("Directories are left on disk."));
            }
            CleanupKind::Remove => {
                lines.push(Line::from(
                    "Each workspace is snapshotted first, so unsnapshotted edits end up in its commit.",
                ));
                lines.push(Line::from(Span::styled(
                    "Ignored files in the deleted directories are lost.",
                    Style::new().red(),
                )));
            }
        }
        if !protected.is_empty() {
            lines.push(Line::from(""));
            for ws in &protected {
                lines.push(Line::from(Span::styled(
                    format!(
                        "Skipping {}: {}.",
                        ws.name,
                        ws.protected_reason().unwrap_or_default()
                    ),
                    Style::new().cyan(),
                )));
            }
        }

        self.pending = Some(PendingCleanup {
            kind,
            names: allowed.iter().map(|ws| ws.name.clone()).collect(),
        });
        let title_style = match kind {
            CleanupKind::Forget => Style::new().bold().cyan(),
            CleanupKind::Remove => Style::new().bold().red(),
        };
        self.popup = ConfirmDialogState::new(
            kind.popup_id(),
            Span::styled(format!(" {} ", kind.title()), title_style),
            Text::from(lines).fg(Color::default()),
        );
        self.popup
            .with_yes_button(ButtonLabel::YES.clone())
            .with_no_button(ButtonLabel::NO.clone())
            .with_listener(Some(self.popup_tx.clone()))
            .open();
        ComponentInputResult::Handled
    }

    /// Run the confirmed cleanup: snapshot, forget, and for a removal delete
    /// the directories. Snapshot failures abort before anything is forgotten.
    fn execute_cleanup(&mut self) -> Result<Option<AppAction>> {
        let Some(pending) = self.pending.take() else {
            return Ok(None);
        };
        let commander = new_commander();
        let all = self.all_workspaces();
        let targets: Vec<Workspace> = pending
            .names
            .iter()
            .filter_map(|name| all.iter().find(|ws| &ws.name == name).cloned())
            .filter(|ws| ws.protected_reason().is_none())
            .collect();
        if targets.is_empty() {
            return Ok(Some(status("Nothing to clean up")));
        }

        for ws in &targets {
            if let Some(root) = ws.root.as_deref()
                && let Err(err) = commander.snapshot_workspace(root)
            {
                return Ok(Some(popup(
                    pending.kind.title(),
                    format!(
                        "Could not snapshot workspace {}:\n\n{err}\n\nNothing was forgotten.",
                        ws.name
                    ),
                )));
            }
        }

        // The snapshot may have turned an idle workspace into one holding
        // work, so judge what survives from a fresh listing.
        let fresh = commander
            .get_workspaces()
            .unwrap_or_else(|_| targets.clone());
        let kept: Vec<String> = targets
            .iter()
            .map(|ws| fresh.iter().find(|f| f.name == ws.name).unwrap_or(ws))
            .filter(|ws| !ws.discardable())
            .map(|ws| ws.short_change_id().to_owned())
            .collect();

        let names: Vec<String> = targets.iter().map(|ws| ws.name.clone()).collect();
        if let Err(err) = commander.forget_workspaces(&names) {
            return Ok(Some(popup(pending.kind.title(), format!("{err}"))));
        }

        let mut deleted = 0;
        let mut failures = Vec::new();
        if pending.kind == CleanupKind::Remove {
            for ws in &targets {
                if ws.root.is_none() {
                    continue;
                }
                match commander.remove_workspace_dir(ws) {
                    Ok(_) => deleted += 1,
                    Err(err) => failures.push(format!("{}: {err:#}", ws.name)),
                }
            }
        }

        for name in &names {
            self.marked.remove(name);
        }
        self.refresh_all();

        let mut message = match pending.kind {
            CleanupKind::Forget => {
                format!("Forgot {}", plural(names.len(), "workspace", "workspaces"))
            }
            CleanupKind::Remove => format!(
                "Removed {} ({} deleted)",
                plural(names.len(), "workspace", "workspaces"),
                plural(deleted, "directory", "directories")
            ),
        };
        if !kept.is_empty() {
            message.push_str(&format!("; kept in the log: {}", kept.join(", ")));
        }
        message.push_str(match pending.kind {
            CleanupKind::Forget => " | u: undo",
            CleanupKind::Remove => " | u: undo (records only)",
        });

        if failures.is_empty() {
            Ok(Some(status(message)))
        } else {
            Ok(Some(AppAction::Multiple(vec![
                status(message),
                popup(
                    "Delete directory",
                    format!(
                        "The workspaces were forgotten, but some directories could not be deleted:\n\n{}",
                        failures.join("\n")
                    ),
                ),
            ])))
        }
    }

    fn update_stale(&mut self) -> ComponentInputResult {
        let Some(ws) = self.selected_workspace() else {
            return ComponentInputResult::Handled;
        };
        let Some(root) = ws.root.as_deref() else {
            return ComponentInputResult::HandledAction(popup(
                "Update stale workspace",
                format!(
                    "Workspace {} has no directory to update. Forget it instead (f).",
                    ws.name
                ),
            ));
        };
        let result = new_commander().update_stale_workspace(root);
        self.refresh_all();
        match result {
            Ok(report) => ComponentInputResult::HandledAction(popup(
                "Update stale workspace",
                format!("{}:\n\n{report}", ws.name),
            )),
            Err(err) => ComponentInputResult::HandledAction(popup(
                "Update stale workspace",
                format!("{err}"),
            )),
        }
    }

    fn undo(&mut self) -> ComponentInputResult {
        let result = new_commander().run_undo();
        self.refresh_all();
        match result {
            Ok(()) => ComponentInputResult::HandledAction(status("Undid the last operation")),
            Err(err) => ComponentInputResult::HandledAction(popup("Undo", format!("{err:#}"))),
        }
    }

    /// Jump to the working-copy commit on the log tab.
    fn view_in_log(&self) -> ComponentInputResult {
        let Some(ws) = self.selected_workspace() else {
            return ComponentInputResult::Handled;
        };
        match new_commander().get_head(ws.commit_id.as_str()) {
            Ok(head) => ComponentInputResult::HandledAction(AppAction::ViewLog(head)),
            Err(err) => {
                ComponentInputResult::HandledAction(popup("Show workspace", format!("{err:#}")))
            }
        }
    }

    /// Open the workspace in the editor: its live directory when there is
    /// one, otherwise its working-copy commit materialized read-only.
    fn open_in_editor(&self) -> ComponentInputResult {
        let Some(ws) = self.selected_workspace() else {
            return ComponentInputResult::Handled;
        };
        let commander = new_commander();
        if let Some(root) = ws.root.as_deref().filter(|root| root.is_dir()) {
            let mut argv = commander.editor_argv();
            argv.push(root.to_string_lossy().into_owned());
            return ComponentInputResult::HandledAction(AppAction::OpenInEditor(EditorCommand {
                argv,
                name: format!("Browse workspace {}", ws.name),
                cleanup: None,
                working_dir: Some(root.to_owned()),
            }));
        }
        let command = commander
            .get_head(ws.commit_id.as_str())
            .and_then(|head| commander.open_revision_tree_command(&head));
        match command {
            Ok(command) => ComponentInputResult::HandledAction(AppAction::OpenInEditor(command)),
            Err(err) => {
                ComponentInputResult::HandledAction(popup("Browse workspace", format!("{err:#}")))
            }
        }
    }

    fn help(&self) -> ComponentInputResult {
        let item = |key: &str, text: &str| (key.to_owned(), text.to_owned());
        ComponentInputResult::HandledAction(AppAction::SetPopup(Some(Box::new(HelpPopup::new(
            vec![
                item("j/k", "scroll down/up"),
                item("J/K", "scroll down/up by ½ page"),
                item("Enter", "show the working-copy commit on the log tab"),
                item("Space", "mark/unmark the workspace for a bulk action"),
                item("A", "mark all listed cleanup candidates (again to unmark)"),
                item(
                    "f",
                    "forget the marked (or selected) workspaces; directories stay on disk",
                ),
                item(
                    "D",
                    "forget the marked (or selected) workspaces and delete their directories",
                ),
                item("U", "update-stale the selected workspace"),
                item("u", "undo the last jj operation"),
                item("a", "toggle between all workspaces and cleanup candidates"),
                item("s", "cycle the sort: state, age, name"),
                item("o", "open the workspace directory in your editor"),
                item("R", "refresh the view"),
            ],
            vec![
                item("Ctrl+e/Ctrl+y", "scroll down/up"),
                item("Ctrl+d/Ctrl+u", "scroll down/up by ½ page"),
                item("w", "toggle diff format"),
            ],
        )))))
    }

    fn title(&self, shown: usize, total: usize) -> String {
        let mut title = if self.candidates_only {
            format!(" Workspaces ({shown} of {total} are cleanup candidates)")
        } else {
            format!(" Workspaces ({total})")
        };
        title.push_str(&format!(" · {}", self.sort.label()));
        if !self.marked.is_empty() {
            title.push_str(&format!(" · {} marked", self.marked.len()));
        }
        title.push(' ');
        title
    }
}

impl Component for WorkspacesTab {
    fn focus(&mut self) -> Result<()> {
        self.refresh_all();
        Ok(())
    }

    fn update(&mut self) -> Result<Option<AppAction>> {
        if let Ok(res) = self.popup_rx.try_recv() {
            let confirmed = res.1.unwrap_or(false)
                && matches!(res.0, FORGET_POPUP_ID | REMOVE_POPUP_ID)
                && self
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.kind.popup_id() == res.0);
            if confirmed {
                return self.execute_cleanup();
            }
            self.pending = None;
        }
        Ok(None)
    }

    fn draw(&mut self, f: &mut Frame<'_>, area: Rect) -> Result<()> {
        let chunks = self.pane_divider.split(area, self.config.layout());

        {
            let total = self.all_workspaces().len();
            let visible = self.visible();
            let current_index = self.selected_index(&visible);
            let title = self.title(visible.len(), total);

            let items: Vec<ListItem> = match self.workspaces_output.as_ref() {
                Ok(_) if visible.is_empty() => {
                    let text = if self.candidates_only {
                        " No cleanup candidates: every workspace is current or holds work"
                    } else {
                        " No workspaces"
                    };
                    vec![ListItem::new(Line::from(text).fg(Color::DarkGray).italic())]
                }
                Ok(_) => visible
                    .iter()
                    .map(|ws| workspace_item(ws, self.marked.contains(&ws.name), self.now))
                    .collect(),
                Err(err) => err
                    .into_text("Error listing workspaces")?
                    .lines
                    .into_iter()
                    .map(ListItem::new)
                    .collect(),
            };

            let list = List::new(items)
                .block(
                    Block::bordered()
                        .title(title)
                        .border_type(BorderType::Rounded),
                )
                .highlight_style(Style::default().bg(self.config.highlight_color()))
                .scroll_padding(3);
            *self.list_state.selected_mut() = current_index;
            f.render_stateful_widget(&list, chunks[0], &mut self.list_state);
            self.list_height = chunks[0].height.saturating_sub(2);
        }

        {
            let mut content = Text::default();
            if let Some(ws) = self.selected_workspace() {
                content.lines.extend(details_header(&ws, self.now));
            }
            match self.show_output.as_ref() {
                Some(Ok(show)) => content.lines.extend(show.into_text()?.lines),
                Some(Err(err)) => content
                    .lines
                    .extend(err.into_text("Error showing the working copy")?.lines),
                None => {}
            }
            self.panel
                .render_context::<TextContent>(content)
                .title(" Details ")
                .draw(f, chunks[1]);
        }

        if self.popup.is_opened() {
            let popup = ConfirmDialog::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Green))
                .selected_button_style(
                    Style::default()
                        .bg(self.config.highlight_color())
                        .underlined(),
                );
            f.render_stateful_widget(popup, area, &mut self.popup);
        }

        Ok(())
    }

    fn input(&mut self, event: Event) -> Result<ComponentInputResult> {
        let Event::Key(key) = event else {
            return Ok(ComponentInputResult::NotHandled);
        };
        if key.kind != KeyEventKind::Press {
            return Ok(ComponentInputResult::Handled);
        }

        if self.popup.is_opened() {
            if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                self.popup = ConfirmDialogState::default();
                self.pending = None;
            } else {
                self.popup.handle(&key);
            }
            return Ok(ComponentInputResult::Handled);
        }

        if self.panel.input(key) {
            return Ok(ComponentInputResult::Handled);
        }

        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.scroll(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll(-1),
            // Two lines per row.
            KeyCode::Char('J') => self.scroll(self.list_height as isize / 4),
            KeyCode::Char('K') => self.scroll((self.list_height as isize / 4).saturating_neg()),
            KeyCode::Char(' ') => self.toggle_mark(),
            KeyCode::Char('A') => self.toggle_candidate_marks(),
            KeyCode::Char('a') => {
                self.candidates_only = !self.candidates_only;
                self.ensure_selection();
                self.refresh_details();
            }
            KeyCode::Char('s') => {
                self.sort = self.sort.next();
            }
            KeyCode::Char('R') | KeyCode::F(5) => self.refresh_all(),
            KeyCode::Char('w') => {
                self.diff_format = self.diff_format.get_next(self.config.diff_tool());
                self.refresh_details();
            }
            KeyCode::Char('f') => return Ok(self.confirm_cleanup(CleanupKind::Forget)),
            KeyCode::Char('D') => return Ok(self.confirm_cleanup(CleanupKind::Remove)),
            KeyCode::Char('U') => return Ok(self.update_stale()),
            KeyCode::Char('u') => return Ok(self.undo()),
            KeyCode::Char('o') => return Ok(self.open_in_editor()),
            KeyCode::Enter => return Ok(self.view_in_log()),
            KeyCode::Char('?') => return Ok(self.help()),
            _ => return Ok(ComponentInputResult::NotHandled),
        }

        Ok(ComponentInputResult::Handled)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::commander::ids::ChangeId;
    use crate::commander::ids::CommitId;

    fn ws(name: &str) -> Workspace {
        Workspace {
            name: name.to_owned(),
            root: Some(PathBuf::from(format!("/tmp/{name}"))),
            change_id: ChangeId("kkmpptxzrspxrzommnulwmwkkqwworpl".to_owned()),
            commit_id: CommitId("0123456789abcdef0123456789abcdef01234567".to_owned()),
            empty: true,
            described: false,
            description: String::new(),
            parents: 1,
            mutable_parents: 1,
            current: false,
            committer_timestamp: 1_000,
            author_timestamp: 500,
            last_touched: Some(1_000),
            hosts_repo: false,
            root_remembered: false,
        }
    }

    fn names(workspaces: &[Workspace]) -> Vec<&str> {
        workspaces.iter().map(|ws| ws.name.as_str()).collect()
    }

    #[test]
    fn humanize_age_picks_sensible_units() {
        assert_eq!(humanize_age(-5), "just now");
        assert_eq!(humanize_age(30), "just now");
        assert_eq!(humanize_age(60), "1 minute ago");
        assert_eq!(humanize_age(59 * 60), "59 minutes ago");
        assert_eq!(humanize_age(3 * 3600), "3 hours ago");
        assert_eq!(humanize_age(86_400), "1 day ago");
        assert_eq!(humanize_age(13 * 86_400), "13 days ago");
        assert_eq!(humanize_age(14 * 86_400), "2 weeks ago");
        assert_eq!(humanize_age(59 * 86_400), "8 weeks ago");
        assert_eq!(humanize_age(60 * 86_400), "2 months ago");
        assert_eq!(humanize_age(3 * 365 * 86_400), "3 years ago");
    }

    #[test]
    fn state_sort_puts_the_most_disposable_first_and_current_last() {
        let mut list = vec![
            Workspace {
                current: true,
                hosts_repo: true,
                ..ws("default")
            },
            Workspace {
                empty: false,
                ..ws("work")
            },
            Workspace {
                mutable_parents: 0,
                last_touched: Some(900),
                ..ws("idle-on-trunk")
            },
            Workspace {
                root: None,
                last_touched: None,
                ..ws("ghost")
            },
            Workspace {
                last_touched: Some(2_000),
                ..ws("idle-newer")
            },
            Workspace {
                last_touched: Some(100),
                ..ws("idle-older")
            },
        ];
        sort_workspaces(&mut list, SortMode::State);
        assert_eq!(
            names(&list),
            vec![
                "ghost",
                "idle-on-trunk",
                "idle-older",
                "idle-newer",
                "work",
                "default"
            ]
        );
    }

    #[test]
    fn age_sort_is_oldest_first_with_ghosts_dated_by_their_commit() {
        let mut list = vec![
            Workspace {
                last_touched: Some(3_000),
                ..ws("recent")
            },
            Workspace {
                root: None,
                last_touched: None,
                committer_timestamp: 50,
                ..ws("ghost")
            },
            Workspace {
                current: true,
                last_touched: Some(1),
                ..ws("default")
            },
            Workspace {
                last_touched: Some(200),
                ..ws("old")
            },
        ];
        sort_workspaces(&mut list, SortMode::Age);
        assert_eq!(names(&list), vec!["ghost", "old", "recent", "default"]);
    }

    #[test]
    fn name_sort_is_alphabetical_regardless_of_state() {
        let mut list = vec![
            Workspace {
                current: true,
                ..ws("default")
            },
            ws("beta"),
            Workspace {
                root: None,
                ..ws("alpha")
            },
        ];
        sort_workspaces(&mut list, SortMode::Name);
        assert_eq!(names(&list), vec!["alpha", "beta", "default"]);
    }

    #[test]
    fn labels_describe_the_base_and_the_age_source() {
        assert_eq!(base_label(&ws("one")), "on a mutable parent");
        assert_eq!(
            base_label(&Workspace {
                mutable_parents: 0,
                ..ws("one")
            }),
            "on an immutable parent"
        );
        assert_eq!(
            base_label(&Workspace {
                parents: 2,
                mutable_parents: 1,
                ..ws("merge")
            }),
            "merge of 2 parents (1 mutable)"
        );
        assert_eq!(
            base_label(&Workspace {
                parents: 2,
                mutable_parents: 0,
                ..ws("merge")
            }),
            "merge of 2 immutable parents"
        );

        let now = 1_000 + 3 * 86_400;
        assert_eq!(age_label(&ws("touched"), now), "3 days ago");
        let ghost = Workspace {
            root: None,
            last_touched: None,
            ..ws("ghost")
        };
        assert_eq!(
            age_label(&ghost, now),
            "~3 days ago",
            "a ghost's age comes from its commit and is flagged approximate"
        );
    }

    #[test]
    fn abbreviate_home_replaces_the_prefix_only() {
        let home = std::env::var_os("HOME").expect("HOME is set in tests");
        let inside = PathBuf::from(&home).join("code").join("ws");
        assert_eq!(abbreviate_home(&inside), "~/code/ws");
        assert_eq!(abbreviate_home(&PathBuf::from(&home)), "~");
        assert_eq!(abbreviate_home(Path::new("/tmp/ws")), "/tmp/ws");
    }
}
