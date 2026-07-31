// `ButtonLabel::YES`/`NO` are consts with interior mutability; cloning them is
// how tui_confirm_dialog's API is meant to be used. Same as in log_tab.
#![expect(clippy::borrow_interior_mutable_const)]

use std::vec;

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
use crate::commander::Commander;
use crate::commander::files::Conflict;
use crate::commander::files::ConflictSide;
use crate::commander::files::File;
use crate::commander::log::Head;
use crate::commander::new_commander;
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

const UNTRACK_POPUP_ID: u16 = 1;

/// Files tab. Shows files in selected change in main panel and selected file diff in details panel
pub struct FilesTab {
    head: Head,
    is_current_head: bool,

    files_output: Result<Vec<File>, CommandError>,
    conflicts_output: Vec<Conflict>,
    files_list_state: ListState,
    files_height: u16,

    pub file: Option<File>,
    diff_panel: DetailsPanel,
    diff_output: Result<Option<String>, CommandError>,
    diff_format: DiffFormat,

    popup: ConfirmDialogState,
    popup_tx: std::sync::mpsc::Sender<Listener>,
    popup_rx: std::sync::mpsc::Receiver<Listener>,

    config: JjConfig,
    pane_divider: PaneDivider,
}

fn get_current_file_index(
    current_file: Option<&File>,
    files_output: Result<&Vec<File>, &CommandError>,
) -> Option<usize> {
    if let (Some(current_file), Ok(files_output)) = (current_file, files_output)
        && let Some(path) = current_file.path.as_ref()
    {
        return files_output
            .iter()
            .position(|file| file.path.as_ref() == Some(path));
    }

    None
}

impl FilesTab {
    #[instrument(level = "info", name = "Initializing files tab", parent = None, skip())]
    pub fn new(head: &Head) -> Result<Self> {
        let head = head.clone();
        let is_current_head = head == new_commander().get_current_head()?;

        let diff_format = get_env().jj_config.diff_format();

        let files_output = new_commander().get_files(&head);
        let conflicts_output = new_commander().get_conflicts(&head.commit_id)?;
        let current_file = files_output
            .as_ref()
            .ok()
            .and_then(|files_output| files_output.first())
            .map(|file| file.to_owned());
        let diff_output = current_file
            .as_ref()
            .map(|current_change| {
                new_commander().get_file_diff(&head, current_change, &diff_format, true)
            })
            .map_or(Ok(None), |r| {
                r.map(|diff| diff.map(|diff| tabs_to_spaces(&diff)))
            });

        let files_list_state = ListState::default().with_selected(get_current_file_index(
            current_file.as_ref(),
            files_output.as_ref(),
        ));

        let config = get_env().jj_config.clone();
        let pane_divider = PaneDivider::new(config.layout_percent());

        let (popup_tx, popup_rx) = std::sync::mpsc::channel();

        Ok(Self {
            head,
            is_current_head,

            files_output,
            file: current_file,
            files_list_state,
            files_height: 0,

            conflicts_output,

            diff_output,
            diff_format,
            diff_panel: DetailsPanel::new(),

            popup: ConfirmDialogState::default(),
            popup_tx,
            popup_rx,

            config,
            pane_divider,
        })
    }

    pub fn set_head(&mut self, head: &Head) -> Result<()> {
        self.head = head.clone();
        self.is_current_head = self.head == new_commander().get_current_head()?;

        self.refresh_files()?;
        self.file = self
            .files_output
            .as_ref()
            .ok()
            .and_then(|files_output| files_output.first())
            .map(|file| file.to_owned());
        self.refresh_diff()?;

        Ok(())
    }

    pub fn get_current_file_index(&self) -> Option<usize> {
        get_current_file_index(self.file.as_ref(), self.files_output.as_ref())
    }

    pub fn refresh_files(&mut self) -> Result<()> {
        self.files_output = new_commander().get_files(&self.head);
        self.conflicts_output = new_commander().get_conflicts(&self.head.commit_id)?;
        Ok(())
    }

    pub fn refresh_diff(&mut self) -> Result<()> {
        let mut commander = new_commander();
        let inner_width = self.diff_panel.columns() as usize;
        commander.limit_width(inner_width);
        self.diff_output = self
            .file
            .as_ref()
            .map(|current_file| {
                commander.get_file_diff(&self.head, current_file, &self.diff_format, true)
            })
            .map_or(Ok(None), |r| {
                r.map(|diff| diff.map(|diff| tabs_to_spaces(&diff)))
            });
        self.diff_panel.scroll_to(0);
        Ok(())
    }

    /// Ask before ignoring and untracking, since the two halves are undone
    /// differently: `jj undo` restores the tracking, but the `.gitignore` line
    /// is a plain file edit that stays behind.
    fn confirm_ignore_and_untrack(&mut self) -> Result<ComponentInputResult> {
        let Some(path) = self.file.as_ref().and_then(Commander::destination_path) else {
            return Ok(ComponentInputResult::Handled);
        };
        let pattern = Commander::gitignore_pattern(path);

        let text = Text::from(vec![
            Line::from("Stop tracking this file and add it to .gitignore?"),
            Line::from(""),
            Line::from(format!("File          : {path}")),
            Line::from(format!(".gitignore    : {pattern}")),
            Line::from(""),
            Line::from("The file itself is kept on disk."),
        ])
        .fg(Color::default());

        self.popup = ConfirmDialogState::new(
            UNTRACK_POPUP_ID,
            Span::styled(" Untrack ", Style::new().bold().cyan()),
            text,
        );
        self.popup
            .with_yes_button(ButtonLabel::YES.clone())
            .with_no_button(ButtonLabel::NO.clone())
            .with_listener(Some(self.popup_tx.clone()))
            .open();
        Ok(ComponentInputResult::Handled)
    }

    /// Add the selected file to `.gitignore` and untrack it, after the confirm
    /// dialog returned yes.
    fn execute_ignore_and_untrack(&mut self) -> Result<Option<AppAction>> {
        let Some(current_file) = self.file.clone() else {
            return Ok(None);
        };
        let path = Commander::destination_path(&current_file)
            .unwrap_or_default()
            .to_owned();

        match new_commander().ignore_and_untrack_file(&current_file) {
            Ok(added) => {
                self.set_head(&new_commander().get_current_head()?)?;
                let message = if added {
                    format!("Untracked {path} and added it to .gitignore | u: undo the untrack")
                } else {
                    format!("Untracked {path} (already in .gitignore) | u: undo")
                };
                Ok(Some(AppAction::SetStatusMessage(message)))
            }
            Err(err) => Ok(Some(AppAction::SetPopup(Some(Box::new(
                MessagePopup::new("Can't untrack file", format!("{err:#}")),
            ))))),
        }
    }

    pub fn restore_file(&mut self) -> Result<()> {
        self.file
            .as_ref()
            .map(|current_file| new_commander().restore_file(current_file))
            .transpose()?;
        Ok(())
    }

    /// Open the selected file in the user's editor. When viewing `@` the
    /// live working-copy file is opened for editing; for any other revision
    /// the file's content at that revision is opened read-only. Returns the
    /// action to run the editor, or a popup action on failure.
    fn open_file(&mut self) -> Result<Option<AppAction>> {
        let Some(current_file) = self.file.clone() else {
            return Ok(None);
        };

        match new_commander().open_file_command(&self.head, &current_file, self.is_current_head) {
            Ok(Some(command)) => Ok(Some(AppAction::OpenInEditor(command))),
            Ok(None) => Ok(None),
            Err(err) => Ok(Some(AppAction::SetPopup(Some(Box::new(
                MessagePopup::new("Can't open file", err.to_string()),
            ))))),
        }
    }

    /// Browse the whole repo as it existed at the revision being shown, by
    /// extracting its file tree to a temp directory and opening that in the
    /// user's editor. Complements [Self::open_file], which opens the single
    /// selected file: the files list only holds the files this revision
    /// *changed*, so browsing the tree is the way to reach everything else.
    fn open_tree(&mut self) -> AppAction {
        match new_commander().open_revision_tree_command(&self.head) {
            Ok(command) => AppAction::OpenInEditor(command),
            Err(err) => AppAction::SetPopup(Some(Box::new(MessagePopup::new(
                "Browse revision",
                format!("{err:#}"),
            )))),
        }
    }

    /// Resolve the selected file's conflict by keeping one side wholesale.
    /// Returns a popup action on failure, `None` on success.
    fn resolve_file(&mut self, side: ConflictSide) -> Result<Option<AppAction>> {
        let popup = |title: &'static str, message: String| {
            Some(AppAction::SetPopup(Some(Box::new(MessagePopup::new(
                title, message,
            )))))
        };

        if self.head.immutable {
            return Ok(popup(
                "Resolve",
                "The conflict cannot be resolved because the change is immutable.".to_owned(),
            ));
        }
        let Some(path) = self.file.as_ref().and_then(|file| file.path.clone()) else {
            return Ok(None);
        };
        if !self
            .conflicts_output
            .iter()
            .any(|conflict| conflict.path == path)
        {
            return Ok(popup(
                "Resolve",
                "The selected file has no conflict to resolve.".to_owned(),
            ));
        }

        if let Err(err) =
            new_commander().run_resolve(self.head.commit_id.as_str(), Some(&path), side)
        {
            return Ok(popup("Resolve", err.to_string()));
        }

        // Resolving rewrote the commit, so re-find the head before refreshing
        self.head = new_commander().get_head_latest(&self.head)?;
        self.refresh_files()?;
        self.refresh_diff()?;
        Ok(None)
    }

    /// Resolve the selected file's conflict in the configured merge editor
    /// (`jj resolve`), with the same guards as [Self::resolve_file].
    fn resolve_file_in_editor(&mut self) -> Result<Option<AppAction>> {
        let popup = |title: &'static str, message: String| {
            Some(AppAction::SetPopup(Some(Box::new(MessagePopup::new(
                title, message,
            )))))
        };

        if self.head.immutable {
            return Ok(popup(
                "Resolve",
                "The conflict cannot be resolved because the change is immutable.".to_owned(),
            ));
        }
        let Some(path) = self.file.as_ref().and_then(|file| file.path.clone()) else {
            return Ok(None);
        };
        if !self
            .conflicts_output
            .iter()
            .any(|conflict| conflict.path == path)
        {
            return Ok(popup(
                "Resolve",
                "The selected file has no conflict to resolve.".to_owned(),
            ));
        }

        Ok(Some(AppAction::RunInteractive(
            Commander::resolve_interactive_command(self.head.commit_id.as_str(), Some(&path)),
        )))
    }

    fn scroll_files(&mut self, scroll: isize) -> Result<()> {
        if let Ok(files) = self.files_output.as_ref() {
            let current_file_index = self.get_current_file_index();
            let next_file = match current_file_index {
                Some(current_file_index) => files.get(
                    current_file_index
                        .saturating_add_signed(scroll)
                        .min(files.len() - 1),
                ),
                None => files.first(),
            }
            .map(|x| x.to_owned());
            if let Some(next_file) = next_file {
                self.file = Some(next_file.to_owned());
                self.refresh_diff()?;
            }
        }
        Ok(())
    }
}

impl Component for FilesTab {
    fn focus(&mut self) -> Result<()> {
        // Re-resolve the head before comparing to @: if the head was just
        // rewritten (e.g. by an interactive resolve), the stale commit id
        // would spuriously compare unequal
        self.head = new_commander().get_head_latest(&self.head)?;
        self.is_current_head = self.head == new_commander().get_current_head()?;
        self.refresh_files()?;
        self.refresh_diff()?;
        Ok(())
    }

    fn update(&mut self) -> Result<Option<AppAction>> {
        if let Ok(res) = self.popup_rx.try_recv()
            && res.1.unwrap_or(false)
            && res.0 == UNTRACK_POPUP_ID
        {
            return self.execute_ignore_and_untrack();
        }

        Ok(None)
    }

    fn draw(
        &mut self,
        f: &mut ratatui::prelude::Frame<'_>,
        area: ratatui::prelude::Rect,
    ) -> Result<()> {
        let chunks = self.pane_divider.split(area, self.config.layout());

        // Draw files
        {
            let current_file_index = self.get_current_file_index();

            let mut lines: Vec<Line> = match self.files_output.as_ref() {
                Ok(files_output) => {
                    let files_lines = files_output
                        .iter()
                        .enumerate()
                        .flat_map(|(i, file)| {
                            file.line
                                .to_text()
                                .unwrap()
                                .iter()
                                .map(|line| {
                                    let mut line = line.to_owned();

                                    // Add padding at start
                                    line.spans.insert(0, Span::from(" "));

                                    if let Some(diff_type) = file.diff_type.as_ref() {
                                        line.spans = line
                                            .spans
                                            .iter_mut()
                                            .map(|span| span.to_owned().fg(diff_type.color()))
                                            .collect();
                                    }

                                    if current_file_index == Some(i) {
                                        line = line.bg(self.config.highlight_color());

                                        line.spans = line
                                            .spans
                                            .iter_mut()
                                            .map(|span| {
                                                span.to_owned().bg(self.config.highlight_color())
                                            })
                                            .collect();
                                    }

                                    line
                                })
                                .collect::<Vec<Line>>()
                        })
                        .collect::<Vec<Line>>();

                    if files_lines.is_empty() {
                        vec![
                            Line::from(" No changed files in change")
                                .fg(Color::DarkGray)
                                .italic(),
                        ]
                    } else {
                        files_lines
                    }
                }
                Err(err) => err.into_text("Error getting files")?.lines,
            };

            let title_change = if self.is_current_head {
                format!("@ ({})", self.head.change_id)
            } else {
                self.head.change_id.as_string()
            };

            if !self.conflicts_output.is_empty() {
                lines.push(Line::default());

                for conflict in &self.conflicts_output {
                    lines.push(Line::raw(format!("C {}", &conflict.path)).fg(Color::Red));
                }
            }

            let files = List::new(lines)
                .block(
                    Block::bordered()
                        .title(" Files for ".to_owned() + &title_change + " ")
                        .border_type(BorderType::Rounded),
                )
                .scroll_padding(3);
            *self.files_list_state.selected_mut() = current_file_index;
            f.render_stateful_widget(&files, chunks[0], &mut self.files_list_state);
            self.files_height = chunks[0].height - 2;

            if let Some(index) = current_file_index
                && files.len() > self.files_height as usize
            {
                let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
                let mut scrollbar_state = ScrollbarState::default()
                    .content_length(files.len())
                    .position(index);

                f.render_stateful_widget(
                    scrollbar,
                    chunks[0].inner(Margin {
                        vertical: 1,
                        horizontal: 0,
                    }),
                    &mut scrollbar_state,
                );
            }
        }

        // Draw diff
        {
            let diff_content = match self.diff_output.as_ref() {
                Ok(Some(diff_content)) => diff_content.into_text()?,
                Ok(None) => Text::default(),
                Err(err) => err.into_text("Error getting diff")?,
            };
            self.diff_panel
                .render_context::<TextContent>(diff_content)
                .title(" Diff ")
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
        if let Event::Key(key) = event {
            if key.kind != KeyEventKind::Press {
                return Ok(ComponentInputResult::Handled);
            }

            // The confirm dialog takes precedence over the tab's own bindings
            if self.popup.is_opened() {
                if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                    self.popup = ConfirmDialogState::default();
                } else {
                    self.popup.handle(&key);
                }
                return Ok(ComponentInputResult::Handled);
            }

            if self.diff_panel.input(key) {
                return Ok(ComponentInputResult::Handled);
            }

            match key.code {
                KeyCode::Char('j') | KeyCode::Down => self.scroll_files(1)?,
                KeyCode::Char('k') | KeyCode::Up => self.scroll_files(-1)?,
                KeyCode::Char('J') => {
                    self.scroll_files(self.files_height as isize / 2)?;
                }
                KeyCode::Char('K') => {
                    self.scroll_files((self.files_height as isize / 2).saturating_neg())?;
                }
                KeyCode::Char('w') => {
                    self.diff_format = self.diff_format.get_next(self.config.diff_tool());
                    self.refresh_diff()?;
                }
                KeyCode::Char('x') => {
                    return self.confirm_ignore_and_untrack();
                }
                KeyCode::Enter => {
                    if let Some(action) = self.open_file()? {
                        return Ok(ComponentInputResult::HandledAction(action));
                    }
                }
                KeyCode::Char('o') => {
                    return Ok(ComponentInputResult::HandledAction(self.open_tree()));
                }
                KeyCode::Char('r') => {
                    if let Err(err) = self.restore_file() {
                        return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                            Some(Box::new(MessagePopup::new(
                                "Can't restore file",
                                err.to_string(),
                            ))),
                        )));
                    }
                    self.set_head(&new_commander().get_current_head()?)?;
                }
                KeyCode::Char('v') => {
                    if let Some(action) = self.resolve_file(ConflictSide::Source)? {
                        return Ok(ComponentInputResult::HandledAction(action));
                    }
                }
                KeyCode::Char('V') => {
                    if let Some(action) = self.resolve_file(ConflictSide::Destination)? {
                        return Ok(ComponentInputResult::HandledAction(action));
                    }
                }
                KeyCode::Char('m') => {
                    if let Some(action) = self.resolve_file_in_editor()? {
                        return Ok(ComponentInputResult::HandledAction(action));
                    }
                }
                KeyCode::Char('R') | KeyCode::F(5) => {
                    self.head = new_commander().get_head_latest(&self.head)?;
                    self.refresh_files()?;
                    self.refresh_diff()?;
                }
                KeyCode::Char('@') => {
                    let head = &new_commander().get_current_head()?;
                    self.set_head(head)?;
                }
                KeyCode::Char('?') => {
                    return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                        Some(Box::new(HelpPopup::new(
                            vec![
                                ("j/k".to_owned(), "scroll down/up".to_owned()),
                                ("J/K".to_owned(), "scroll down by ½ page".to_owned()),
                                (
                                    "Enter".to_owned(),
                                    "open file in editor (read-only for non-@ revisions)"
                                        .to_owned(),
                                ),
                                (
                                    "o".to_owned(),
                                    "browse the whole repo at this revision in your editor"
                                        .to_owned(),
                                ),
                                (
                                    "x".to_owned(),
                                    "stop tracking the file and add it to .gitignore (keeps it on disk)"
                                        .to_owned(),
                                ),
                                ("r".to_owned(), "restore file".to_owned()),
                                (
                                    "v".to_owned(),
                                    "resolve conflict keeping the rebased/squashed revision's version"
                                        .to_owned(),
                                ),
                                (
                                    "V".to_owned(),
                                    "resolve conflict keeping the rebase/squash destination's version"
                                        .to_owned(),
                                ),
                                (
                                    "m".to_owned(),
                                    "resolve conflict in the merge editor".to_owned(),
                                ),
                                ("@".to_owned(), "view current change files".to_owned()),
                                ("R/F5".to_owned(), "refresh the view".to_owned()),
                            ],
                            vec![
                                ("Ctrl+e/Ctrl+y".to_owned(), "scroll down/up".to_owned()),
                                (
                                    "Ctrl+d/Ctrl+u".to_owned(),
                                    "scroll down/up by ½ page".to_owned(),
                                ),
                                (
                                    "Ctrl+f/Ctrl+b".to_owned(),
                                    "scroll down/up by page".to_owned(),
                                ),
                                ("w".to_owned(), "toggle diff format".to_owned()),
                                ("W".to_owned(), "toggle wrapping".to_owned()),
                            ],
                        ))),
                    )));
                }
                _ => return Ok(ComponentInputResult::NotHandled),
            };
        }

        if let Event::Mouse(mouse) = event {
            if self.pane_divider.handle_mouse(mouse, self.config.layout()) {
                return Ok(ComponentInputResult::Handled);
            }
            if self.diff_panel.input_mouse(mouse) {
                return Ok(ComponentInputResult::Handled);
            }
            return Ok(ComponentInputResult::NotHandled);
        }

        Ok(ComponentInputResult::Handled)
    }
}
