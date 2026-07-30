/** The environment configures the application.

It is a combination of
- configuration files
- environment variables
- command line arguments
*/
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use minijinja::Environment;
use minijinja::context;
use ratatui::style::Color;
use serde::Deserialize;

use crate::commander::RemoveEndLine;
use crate::commander::get_output_args;
use crate::keybinds::KeybindsConfig;
use crate::keybinds::Shortcut;

/// Singleton holding application environment
static ENV: OnceLock<Env> = OnceLock::new();

/// Set application environment. Panics if called twice
pub fn set_env(env: Env) {
    ENV.set(env).expect("set_env must only be called once");
}

/// Get application environment. Panics if not set first
pub fn get_env() -> &'static Env {
    ENV.get().unwrap()
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "kebab-case", default)]
pub struct JjConfig {
    pub jjscope: JjConfigJjscope,
    pub ui: JjConfigUi,
    pub templates: JjConfigTemplates,
}

/// A user-defined rewrite of a change's description, bound to its own key.
///
/// The template is a [MiniJinja](https://docs.rs/minijinja) template rendered
/// with the original description in scope as `desc`.
#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct DescriptionTransform {
    /// Shown in the help popup and in the status message after applying.
    pub name: String,
    /// The shortcut that applies this transform in the log tab.
    pub key: Shortcut,
    /// The new description, as a Jinja template over `desc`.
    pub template: String,
}

/// The template environment, with the extra filters jjscope provides on top of
/// MiniJinja's builtins.
///
/// MiniJinja follows Jinja rather than Python: there are no string *methods*,
/// so prefix checks are the `startingwith`/`endingwith` tests and string
/// operations are filters. `removeprefix`/`removesuffix` are added here under
/// their Python names because stripping an affix is the other half of every
/// toggle-style transform, and spelling it `| replace(p, "")` would also strip
/// occurrences from the middle of the description.
fn template_env() -> Environment<'static> {
    let mut env = Environment::new();
    env.add_filter("removeprefix", |s: &str, prefix: &str| {
        s.strip_prefix(prefix).unwrap_or(s).to_string()
    });
    env.add_filter("removesuffix", |s: &str, suffix: &str| {
        s.strip_suffix(suffix).unwrap_or(s).to_string()
    });
    env
}

impl DescriptionTransform {
    /// Render the template with `description` in scope as `desc`.
    ///
    /// The result is trimmed: templates spanning several lines for readability
    /// would otherwise pick up the newlines around their `{% if %}` blocks, and
    /// jj descriptions are whitespace-sensitive.
    pub fn apply(&self, description: &str) -> Result<String> {
        let rendered = template_env()
            .render_str(&self.template, context! { desc => description })
            .with_context(|| {
                format!(
                    "Failed rendering the \"{}\" description transform",
                    self.name
                )
            })?;
        Ok(rendered.trim().to_string())
    }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "kebab-case", default)]
pub struct JjConfigJjscope {
    highlight_color: Color,
    diff_format: Option<DiffFormat>,
    diff_tool: Option<String>,
    bookmark_template: Option<String>,
    layout: JJLayout,
    layout_percent: u16,
    keybinds: Option<KeybindsConfig>,
    description_transforms: Vec<DescriptionTransform>,
}

impl Default for JjConfigJjscope {
    fn default() -> Self {
        Self {
            highlight_color: Color::Rgb(50, 50, 150),
            layout_percent: 50,
            // Standard defaults for the rest
            diff_format: None,
            diff_tool: None,
            bookmark_template: None,
            layout: JJLayout::default(),
            keybinds: None,
            description_transforms: Vec::new(),
        }
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "kebab-case", default)]
pub struct JjConfigUi {
    diff: JjConfigUiDiff,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "kebab-case", default)]
pub struct JjConfigUiDiff {
    format: Option<DiffFormat>,
    tool: Option<toml::Value>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct JjConfigTemplates {
    git_push_bookmark: Option<String>,
}

impl JjConfig {
    pub fn diff_format(&self) -> DiffFormat {
        self.jjscope
            .diff_format
            .clone()
            .or_else(|| self.ui.diff.format.clone())
            .or_else(|| self.diff_tool().map(DiffFormat::DiffTool))
            .unwrap_or(DiffFormat::ColorWords)
    }

    pub fn diff_tool(&self) -> Option<Option<String>> {
        match self.jjscope.diff_tool.clone() {
            tool @ Some(_) => Some(tool),
            _ if self.ui.diff.tool.is_some() => Some(None),
            _ => None,
        }
    }

    pub fn highlight_color(&self) -> Color {
        self.jjscope.highlight_color
    }

    pub fn bookmark_template(&self) -> String {
        self.jjscope
            .bookmark_template
            .clone()
            .or(self.templates.git_push_bookmark.clone())
            .unwrap_or("'push-' ++ change_id.short()".to_string())
    }

    pub fn layout(&self) -> JJLayout {
        self.jjscope.layout
    }

    pub fn layout_percent(&self) -> u16 {
        self.jjscope.layout_percent
    }

    pub fn keybinds(&self) -> Option<&KeybindsConfig> {
        self.jjscope.keybinds.as_ref()
    }

    pub fn description_transforms(&self) -> &[DescriptionTransform] {
        &self.jjscope.description_transforms
    }
}

#[derive(Debug, Clone)]
pub struct Env {
    pub jj_config: JjConfig,
    pub root: String,
    pub default_revset: Option<String>,
    pub jj_bin: String,
}

impl Env {
    pub fn new(path: PathBuf, default_revset: Option<String>, jj_bin: String) -> Result<Env> {
        // Get jj repository root
        let root_output = Command::new(&jj_bin)
            .arg("root")
            .args(get_output_args(false, true))
            .current_dir(&path)
            .output()?;
        if !root_output.status.success() {
            bail!("No jj repository found in {}", path.to_str().unwrap_or(""))
        }
        let root = String::from_utf8(root_output.stdout)?.remove_end_line();

        // Read/parse jj config
        let cfg = Command::new(&jj_bin)
            .arg("config")
            .arg("list")
            .args(get_output_args(false, true))
            .current_dir(&root)
            .output()
            .context("Failed to get jj config")?
            .stdout;
        let jj_config: JjConfig = toml::from_slice(&cfg).context("Failed to parse jj config")?;

        Ok(Env {
            root,
            jj_config,
            default_revset,
            jj_bin,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Default, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum DiffFormat {
    #[default]
    ColorWords,
    Git,
    DiffTool(Option<String>),
    // Unused
    Summary,
    Stat,
}

impl DiffFormat {
    pub fn get_next(&self, diff_tool: Option<Option<String>>) -> DiffFormat {
        match self {
            DiffFormat::ColorWords => DiffFormat::Git,
            DiffFormat::Git => {
                if let Some(diff_tool) = diff_tool {
                    DiffFormat::DiffTool(diff_tool)
                } else {
                    DiffFormat::ColorWords
                }
            }
            _ => DiffFormat::ColorWords,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Default, Copy, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum JJLayout {
    #[default]
    Horizontal,
    Vertical,
}

// Impl into for JJLayout to ratatui's Direction
impl From<JJLayout> for ratatui::layout::Direction {
    fn from(layout: JJLayout) -> Self {
        match layout {
            JJLayout::Horizontal => ratatui::layout::Direction::Horizontal,
            JJLayout::Vertical => ratatui::layout::Direction::Vertical,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    /// The toggle from the README: archive an unarchived change, unarchive an
    /// archived one.
    const TOGGLE: &str = r#"
        {%- if desc is startingwith("archived: ") -%}
          {{ desc | removeprefix("archived: ") }}
        {%- else -%}
          archived: {{ desc }}
        {%- endif -%}
    "#;

    fn transform(template: &str) -> DescriptionTransform {
        DescriptionTransform {
            name: "test".to_string(),
            key: Shortcut::from_str("shift+g").unwrap(),
            template: template.to_string(),
        }
    }

    fn apply(template: &str, description: &str) -> String {
        transform(template).apply(description).unwrap()
    }

    #[test]
    fn description_transform_apply() {
        // `desc` holds the original description.
        assert_eq!(
            apply("archived: {{ desc }}", "my change"),
            "archived: my change"
        );
        // A template may use the original more than once, or not at all.
        assert_eq!(apply("{{ desc }} / {{ desc }}", "x"), "x / x");
        assert_eq!(apply("fixed", "my change"), "fixed");
        // Jinja syntax in the description is inserted verbatim, not re-rendered.
        assert_eq!(
            apply("archived: {{ desc }}", "a {{ desc }} b"),
            "archived: a {{ desc }} b"
        );
        // Multi-line descriptions are preserved.
        assert_eq!(
            apply("archived: {{ desc }}", "subject\n\nbody"),
            "archived: subject\n\nbody"
        );
    }

    #[test]
    fn description_transform_toggle() {
        assert_eq!(apply(TOGGLE, "my change"), "archived: my change");
        assert_eq!(apply(TOGGLE, "archived: my change"), "my change");
        // Toggling twice is the identity.
        assert_eq!(apply(TOGGLE, &apply(TOGGLE, "my change")), "my change");
    }

    #[test]
    fn description_transform_trims_result() {
        // Templates laid out over several lines for readability pick up the
        // newlines around their blocks; the result is trimmed so descriptions
        // do not gain stray blank lines.
        assert_eq!(
            apply(
                "\n  {% if true %}\n  archived: {{ desc }}\n  {% endif %}\n",
                "x"
            ),
            "archived: x"
        );
        // Interior whitespace is left alone.
        assert_eq!(apply("{{ desc }}", "subject\n\nbody"), "subject\n\nbody");
    }

    #[test]
    fn description_transform_extra_filters() {
        assert_eq!(
            apply(r#"{{ desc | removeprefix("wip: ") }}"#, "wip: x"),
            "x"
        );
        // A missing affix leaves the description alone.
        assert_eq!(apply(r#"{{ desc | removeprefix("wip: ") }}"#, "x"), "x");
        assert_eq!(
            apply(r#"{{ desc | removesuffix(" (wip)") }}"#, "x (wip)"),
            "x"
        );
        assert_eq!(apply(r#"{{ desc | removesuffix(" (wip)") }}"#, "x"), "x");
        // Unlike `| replace`, only the affix is removed.
        assert_eq!(
            apply(r#"{{ desc | removeprefix("a: ") }}"#, "a: keep a: this"),
            "keep a: this"
        );
    }

    #[test]
    fn description_transform_errors_are_reported() {
        // A syntax error surfaces as an error rather than a mangled description.
        let err = transform("{{ desc").apply("x").unwrap_err();
        assert!(
            format!("{err:#}").contains("syntax error"),
            "unexpected error: {err:#}"
        );
        // So does calling something that does not exist.
        assert!(transform("{{ desc.bogus() }}").apply("x").is_err());
        // The message names the transform, so the popup identifies which one failed.
        assert!(
            format!("{err:#}").contains("test"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn parse_description_transforms_config() {
        let config: JjConfig = toml::from_str(
            r#"
            [[jjscope.description-transforms]]
            name = "archive"
            key = "shift+g"
            template = "archived: {{ desc }}"

            [[jjscope.description-transforms]]
            name = "wip"
            key = "ctrl+w"
            template = "wip: {{ desc }}"
            "#,
        )
        .unwrap();

        let transforms = config.description_transforms();
        assert_eq!(transforms.len(), 2);
        assert_eq!(transforms[0].name, "archive");
        assert_eq!(transforms[0].key, Shortcut::from_str("shift+g").unwrap());
        assert_eq!(
            transforms[0].apply("a change").unwrap(),
            "archived: a change"
        );
        assert_eq!(transforms[1].key, Shortcut::from_str("ctrl+w").unwrap());
    }

    #[test]
    fn description_transforms_default_to_empty() {
        let config: JjConfig = toml::from_str("").unwrap();
        assert!(config.description_transforms().is_empty());
    }
}
