//! Repository configuration (spec §22).
//!
//! [`Config::load`] reads `.rivet/config.toml` under a discovered root. Unknown
//! keys are rejected by serde's `deny_unknown_fields`; ranges and enum values
//! are validated so errors name the key and the offending value. Absent config
//! means built-in defaults.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::Deserialize;
use serde::de::{Deserializer, Error as DeError};

/// Default dependency/build exclusions (spec §22).
pub const DEFAULT_EXCLUDE: [&str; 4] = ["vendor/**", "node_modules/**", "dist/**", "build/**"];

/// Default languages enabled for indexing (spec §22).
pub const DEFAULT_LANGUAGES: [&str; 2] = ["php", "typescript"];

/// Default test path globs used for context (spec §22).
pub const DEFAULT_TEST_GLOBS: [&str; 3] = ["tests/**", "**/*.test.ts", "**/*.spec.ts"];

/// The exact spec §22 `config.toml` with built-in default values.
const DEFAULT_TOML: &str = "\
[index]
respect_gitignore = true
exclude = [\"vendor/**\", \"node_modules/**\", \"dist/**\", \"build/**\"]
max_file_size_kb = 1024
freshness = \"content\"                 # content | metadata

[languages]
enabled = [\"php\", \"typescript\"]

[context]
default_token_budget = 4000
include_tests = true
test_globs = [\"tests/**\", \"**/*.test.ts\", \"**/*.spec.ts\"]
max_depth = 2
collapse = \"auto\"

[output]
default_limit = 50
";

/// How freshness is verified before answering a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Freshness {
    /// Read and hash source content.
    #[default]
    Content,
    /// Trust size and modification time.
    Metadata,
}

impl Freshness {
    /// The contract spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Freshness::Content => "content",
            Freshness::Metadata => "metadata",
        }
    }
}

impl FromStr for Freshness {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "content" => Ok(Freshness::Content),
            "metadata" => Ok(Freshness::Metadata),
            _ => Err(()),
        }
    }
}

impl fmt::Display for Freshness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How context source bodies are collapsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Collapse {
    /// Collapse only when the budget requires it.
    #[default]
    Auto,
    /// Always collapse.
    Always,
    /// Never collapse.
    Never,
}

impl Collapse {
    /// The contract spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Collapse::Auto => "auto",
            Collapse::Always => "always",
            Collapse::Never => "never",
        }
    }
}

impl FromStr for Collapse {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "auto" => Ok(Collapse::Auto),
            "always" => Ok(Collapse::Always),
            "never" => Ok(Collapse::Never),
            _ => Err(()),
        }
    }
}

impl fmt::Display for Collapse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The `[index]` table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexConfig {
    /// Whether repository-local Git ignore rules are applied.
    #[serde(default = "default_true")]
    pub respect_gitignore: bool,
    /// Additional path globs to exclude.
    #[serde(default = "default_exclude")]
    pub exclude: Vec<String>,
    /// Maximum eligible regular-file size in kibibytes.
    #[serde(
        default = "default_max_file_size_kb",
        deserialize_with = "deserialize_max_file_size_kb"
    )]
    pub max_file_size_kb: u64,
    /// Freshness verification mode.
    #[serde(default, deserialize_with = "deserialize_freshness")]
    pub freshness: Freshness,
}

impl Default for IndexConfig {
    fn default() -> IndexConfig {
        IndexConfig {
            respect_gitignore: true,
            exclude: default_exclude(),
            max_file_size_kb: default_max_file_size_kb(),
            freshness: Freshness::Content,
        }
    }
}

/// The `[languages]` table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguagesConfig {
    /// Languages to extract, in preference order.
    #[serde(default = "default_enabled", deserialize_with = "deserialize_enabled")]
    pub enabled: Vec<String>,
}

impl Default for LanguagesConfig {
    fn default() -> LanguagesConfig {
        LanguagesConfig {
            enabled: default_enabled(),
        }
    }
}

/// The `[context]` table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextConfig {
    /// Default source-token budget for `context`.
    #[serde(default = "default_token_budget")]
    pub default_token_budget: u32,
    /// Whether test callers are eligible context candidates.
    #[serde(default = "default_true")]
    pub include_tests: bool,
    /// Path globs treated as test files.
    #[serde(default = "default_test_globs")]
    pub test_globs: Vec<String>,
    /// Relationship traversal depth; 1 or 2.
    #[serde(
        default = "default_max_depth",
        deserialize_with = "deserialize_max_depth"
    )]
    pub max_depth: u8,
    /// Collapse mode for rendered bodies.
    #[serde(default, deserialize_with = "deserialize_collapse")]
    pub collapse: Collapse,
}

impl Default for ContextConfig {
    fn default() -> ContextConfig {
        ContextConfig {
            default_token_budget: default_token_budget(),
            include_tests: true,
            test_globs: default_test_globs(),
            max_depth: default_max_depth(),
            collapse: Collapse::Auto,
        }
    }
}

/// The `[output]` table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputConfig {
    /// Default page size for list output; 1 through 1000.
    #[serde(
        default = "default_limit",
        deserialize_with = "deserialize_default_limit"
    )]
    pub default_limit: u32,
}

impl Default for OutputConfig {
    fn default() -> OutputConfig {
        OutputConfig {
            default_limit: default_limit(),
        }
    }
}

/// The full `.rivet/config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// `[index]` settings.
    #[serde(default)]
    pub index: IndexConfig,
    /// `[languages]` settings.
    #[serde(default)]
    pub languages: LanguagesConfig,
    /// `[context]` settings.
    #[serde(default)]
    pub context: ContextConfig,
    /// `[output]` settings.
    #[serde(default)]
    pub output: OutputConfig,
}

impl Config {
    /// Loads and validates `.rivet/config.toml` under `root`.
    ///
    /// A missing file yields [`Config::default`]. A symlinked config file is
    /// refused (spec §27), as is a config path that is not a regular file.
    pub fn load(root: &Path) -> Result<Config, ConfigError> {
        let path = root.join(".rivet").join("config.toml");
        match std::fs::symlink_metadata(&path) {
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Config::default());
            }
            Err(source) => return Err(ConfigError::Io { path, source }),
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ConfigError::Symlinked { path });
            }
            Ok(metadata) if !metadata.is_file() => return Err(ConfigError::NotAFile { path }),
            Ok(_) => {}
        }

        let text = std::fs::read_to_string(&path).map_err(|source| ConfigError::Io {
            path: path.clone(),
            source,
        })?;
        parse(&text).map_err(|message| ConfigError::Invalid {
            path: Some(path),
            message,
        })
    }

    /// Parses config text with the same rules as [`Config::load`].
    pub fn from_toml(text: &str) -> Result<Config, ConfigError> {
        parse(text).map_err(|message| ConfigError::Invalid {
            path: None,
            message,
        })
    }

    /// The exact spec §22 TOML with built-in defaults, for `init` to write.
    pub fn default_toml() -> String {
        DEFAULT_TOML.to_string()
    }

    /// A stable, hash-free canonical serialization of the indexing-affecting
    /// fields (`[index]` and `[languages]`) for cache invalidation.
    ///
    /// Keys are sorted and values are escaped, so TOML key reordering does not
    /// change the fingerprint while any semantic change does.
    /// `languages.enabled` is a set, so its list is sorted;
    /// `index.exclude` is order-sensitive (gitignore-style negations depend on
    /// position), so its declared order is preserved. Context/output settings
    /// are excluded because they do not affect the stored index.
    pub fn fingerprint(&self) -> String {
        let lines = [
            format!("index.exclude={}", format_list_ordered(&self.index.exclude)),
            format!("index.freshness={}", self.index.freshness.as_str()),
            format!("index.max_file_size_kb={}", self.index.max_file_size_kb),
            format!("index.respect_gitignore={}", self.index.respect_gitignore),
            format!("languages.enabled={}", format_list(&self.languages.enabled)),
        ];
        lines.join("\n")
    }
}

/// Parses TOML into a validated [`Config`], returning the error message.
fn parse(text: &str) -> Result<Config, String> {
    toml::from_str::<Config>(text).map_err(|source| source.to_string())
}

/// Formats a list of strings as a sorted, escaped canonical array.
fn format_list(values: &[String]) -> String {
    let mut sorted: Vec<&str> = values.iter().map(String::as_str).collect();
    sorted.sort_unstable();

    let mut out = String::from("[");
    for (index, value) in sorted.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        push_quoted(&mut out, value);
    }
    out.push(']');
    out
}

/// Formats a list of strings as an escaped canonical array in declared order.
///
/// Used for order-sensitive gitignore-style globs, where the position of a
/// negation pattern changes its meaning.
fn format_list_ordered(values: &[String]) -> String {
    let mut out = String::from("[");
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        push_quoted(&mut out, value);
    }
    out.push(']');
    out
}

/// Appends one double-quoted, escaped string.
fn push_quoted(out: &mut String, value: &str) {
    use std::fmt::Write as _;

    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", control as u32);
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

fn default_true() -> bool {
    true
}

fn default_exclude() -> Vec<String> {
    DEFAULT_EXCLUDE
        .iter()
        .map(|glob| (*glob).to_string())
        .collect()
}

fn default_max_file_size_kb() -> u64 {
    1024
}

fn default_enabled() -> Vec<String> {
    DEFAULT_LANGUAGES
        .iter()
        .map(|language| (*language).to_string())
        .collect()
}

fn default_token_budget() -> u32 {
    4000
}

fn default_test_globs() -> Vec<String> {
    DEFAULT_TEST_GLOBS
        .iter()
        .map(|glob| (*glob).to_string())
        .collect()
}

fn default_max_depth() -> u8 {
    2
}

fn default_limit() -> u32 {
    50
}

/// Deserializes `freshness`, naming the key and value on an unknown variant.
fn deserialize_freshness<'de, D>(deserializer: D) -> Result<Freshness, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    Freshness::from_str(&value).map_err(|()| {
        D::Error::custom(format!(
            "invalid value for `index.freshness`: {value:?} (expected \"content\" or \"metadata\")"
        ))
    })
}

/// Deserializes `collapse`, naming the key and value on an unknown variant.
fn deserialize_collapse<'de, D>(deserializer: D) -> Result<Collapse, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    Collapse::from_str(&value).map_err(|()| {
        D::Error::custom(format!(
            "invalid value for `context.collapse`: {value:?} (expected \"auto\", \"always\", or \"never\")"
        ))
    })
}

/// Deserializes `max_file_size_kb`, rejecting zero with the key and value.
fn deserialize_max_file_size_kb<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = i64::deserialize(deserializer)?;
    if value > 0 {
        Ok(value as u64)
    } else {
        Err(D::Error::custom(format!(
            "invalid value for `index.max_file_size_kb`: {value} (expected a positive number)"
        )))
    }
}

/// Deserializes `enabled`, rejecting an empty list with the key.
fn deserialize_enabled<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Vec::<String>::deserialize(deserializer)?;
    if value.is_empty() {
        Err(D::Error::custom(
            "invalid value for `languages.enabled`: [] (expected at least one language)",
        ))
    } else {
        Ok(value)
    }
}

/// Deserializes `max_depth`, rejecting values other than 1 or 2 by name.
fn deserialize_max_depth<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: Deserializer<'de>,
{
    let value = i64::deserialize(deserializer)?;
    match value {
        1 | 2 => Ok(value as u8),
        _ => Err(D::Error::custom(format!(
            "invalid value for `context.max_depth`: {value} (expected 1 or 2)"
        ))),
    }
}

/// Deserializes `default_limit`, rejecting values outside 1..=1000 by name.
fn deserialize_default_limit<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let value = i64::deserialize(deserializer)?;
    if (1..=1000).contains(&value) {
        Ok(value as u32)
    } else {
        Err(D::Error::custom(format!(
            "invalid value for `output.default_limit`: {value} (expected 1 through 1000)"
        )))
    }
}

/// Why configuration could not be loaded.
#[derive(Debug)]
pub enum ConfigError {
    /// The config file could not be read.
    Io {
        /// The config path that failed.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The config path is a symlink, which is refused (spec §27).
    Symlinked {
        /// The offending symlink path.
        path: PathBuf,
    },
    /// The config path exists but is not a regular file.
    NotAFile {
        /// The offending path.
        path: PathBuf,
    },
    /// The TOML is malformed or violates the schema or range rules.
    Invalid {
        /// The config path when loaded from disk; `None` for in-memory parsing.
        path: Option<PathBuf>,
        /// A message naming the offending key and value.
        message: String,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io { path, source } => {
                write!(f, "cannot read config {}: {source}", path.display())
            }
            ConfigError::Symlinked { path } => {
                write!(f, "refusing to read symlinked config {}", path.display())
            }
            ConfigError::NotAFile { path } => {
                write!(f, "config {} is not a regular file", path.display())
            }
            ConfigError::Invalid {
                path: Some(path),
                message,
            } => write!(f, "invalid config at {}: {message}", path.display()),
            ConfigError::Invalid {
                path: None,
                message,
            } => write!(f, "invalid config: {message}"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Collapse, Config, ConfigError, Freshness};
    use crate::test_support::TempDir;
    use std::fs;

    #[test]
    fn missing_config_yields_defaults() {
        let temp = TempDir::new("config-missing");
        let config = Config::load(temp.path()).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn defaults_match_spec_section_22() {
        let config = Config::default();
        assert!(config.index.respect_gitignore);
        assert_eq!(
            config.index.exclude,
            vec!["vendor/**", "node_modules/**", "dist/**", "build/**"]
        );
        assert_eq!(config.index.max_file_size_kb, 1024);
        assert_eq!(config.index.freshness, Freshness::Content);
        assert_eq!(config.languages.enabled, vec!["php", "typescript"]);
        assert_eq!(config.context.default_token_budget, 4000);
        assert!(config.context.include_tests);
        assert_eq!(
            config.context.test_globs,
            vec!["tests/**", "**/*.test.ts", "**/*.spec.ts"]
        );
        assert_eq!(config.context.max_depth, 2);
        assert_eq!(config.context.collapse, Collapse::Auto);
        assert_eq!(config.output.default_limit, 50);
    }

    #[test]
    fn default_toml_parses_back_to_defaults() {
        let parsed = Config::from_toml(&Config::default_toml()).unwrap();
        assert_eq!(parsed, Config::default());
    }

    #[test]
    fn loads_present_config_from_disk() {
        let temp = TempDir::new("config-present");
        fs::create_dir_all(temp.path().join(".rivet")).unwrap();
        fs::write(
            temp.path().join(".rivet/config.toml"),
            "[index]\nmax_file_size_kb = 2048\nfreshness = \"metadata\"\n[languages]\nenabled = [\"php\"]\n",
        )
        .unwrap();

        let config = Config::load(temp.path()).unwrap();
        assert_eq!(config.index.max_file_size_kb, 2048);
        assert_eq!(config.index.freshness, Freshness::Metadata);
        assert_eq!(config.languages.enabled, vec!["php"]);
        // Omitted tables and keys fall back to defaults.
        assert_eq!(config.context, super::ContextConfig::default());
    }

    #[test]
    fn unknown_key_is_rejected_with_key_name() {
        let error = Config::from_toml("[index]\nrespect_gitignores = true\n").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("respect_gitignores"), "{message}");
        assert!(matches!(error, ConfigError::Invalid { .. }));
    }

    #[test]
    fn unknown_table_is_rejected_with_key_name() {
        let error = Config::from_toml("[indexes]\nrespect_gitignore = true\n").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("indexes"), "{message}");
    }

    #[test]
    fn max_depth_three_is_rejected() {
        let error = Config::from_toml("[context]\nmax_depth = 3\n").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("max_depth"), "{message}");
        assert!(message.contains('3'), "{message}");
    }

    #[test]
    fn max_depth_zero_is_rejected() {
        let error = Config::from_toml("[context]\nmax_depth = 0\n").unwrap_err();
        assert!(error.to_string().contains("max_depth"));
    }

    #[test]
    fn default_limit_zero_is_rejected() {
        let error = Config::from_toml("[output]\ndefault_limit = 0\n").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("default_limit"), "{message}");
        assert!(message.contains('0'), "{message}");
    }

    #[test]
    fn default_limit_above_one_thousand_is_rejected() {
        let error = Config::from_toml("[output]\ndefault_limit = 1001\n").unwrap_err();
        assert!(error.to_string().contains("default_limit"));
    }

    #[test]
    fn freshness_fast_is_rejected() {
        let error = Config::from_toml("[index]\nfreshness = \"fast\"\n").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("freshness"), "{message}");
        assert!(message.contains("fast"), "{message}");
    }

    #[test]
    fn collapse_unknown_value_is_rejected() {
        let error = Config::from_toml("[context]\ncollapse = \"sometimes\"\n").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("collapse"), "{message}");
        assert!(message.contains("sometimes"), "{message}");
    }

    #[test]
    fn zero_max_file_size_is_rejected() {
        let error = Config::from_toml("[index]\nmax_file_size_kb = 0\n").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("max_file_size_kb"), "{message}");
    }

    #[test]
    fn empty_enabled_languages_is_rejected() {
        let error = Config::from_toml("[languages]\nenabled = []\n").unwrap_err();
        assert!(error.to_string().contains("enabled"));
    }

    #[test]
    fn fingerprint_ignores_toml_key_and_list_order() {
        // TOML key reordering is ignored.
        let first = Config::from_toml(
            "[index]\nfreshness = \"metadata\"\nrespect_gitignore = false\nexclude = [\"b/**\", \"a/**\"]\nmax_file_size_kb = 5\n[languages]\nenabled = [\"typescript\", \"php\"]\n",
        )
        .unwrap();
        let reordered_keys = Config::from_toml(
            "[languages]\nenabled = [\"typescript\", \"php\"]\n[index]\nexclude = [\"b/**\", \"a/**\"]\nmax_file_size_kb = 5\nrespect_gitignore = false\nfreshness = \"metadata\"\n",
        )
        .unwrap();
        assert_eq!(first.fingerprint(), reordered_keys.fingerprint());

        // `languages.enabled` is a set: reordering its list is ignored.
        let reordered_languages = Config::from_toml(
            "[index]\nfreshness = \"metadata\"\nrespect_gitignore = false\nexclude = [\"b/**\", \"a/**\"]\nmax_file_size_kb = 5\n[languages]\nenabled = [\"php\", \"typescript\"]\n",
        )
        .unwrap();
        assert_eq!(first.fingerprint(), reordered_languages.fingerprint());

        // `index.exclude` is order-sensitive: gitignore-style negations depend on
        // position, so reordering the list changes the fingerprint.
        let reordered_exclude = Config::from_toml(
            "[index]\nfreshness = \"metadata\"\nrespect_gitignore = false\nexclude = [\"a/**\", \"b/**\"]\nmax_file_size_kb = 5\n[languages]\nenabled = [\"typescript\", \"php\"]\n",
        )
        .unwrap();
        assert_ne!(first.fingerprint(), reordered_exclude.fingerprint());
    }

    #[test]
    fn fingerprint_changes_when_exclude_changes() {
        let first = Config::from_toml("[index]\nexclude = [\"a/**\"]\n").unwrap();
        let second = Config::from_toml("[index]\nexclude = [\"b/**\"]\n").unwrap();

        assert_ne!(first.fingerprint(), second.fingerprint());
    }

    #[test]
    fn fingerprint_changes_when_languages_change() {
        let first = Config::from_toml("[languages]\nenabled = [\"php\"]\n").unwrap();
        let second = Config::from_toml("[languages]\nenabled = [\"typescript\"]\n").unwrap();

        assert_ne!(first.fingerprint(), second.fingerprint());
    }

    #[test]
    fn fingerprint_ignores_context_and_output_tables() {
        let first =
            Config::from_toml("[context]\nmax_depth = 1\n[output]\ndefault_limit = 10\n").unwrap();
        let second =
            Config::from_toml("[context]\nmax_depth = 2\n[output]\ndefault_limit = 999\n").unwrap();

        assert_eq!(first.fingerprint(), second.fingerprint());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_config_file_is_rejected() {
        let temp = TempDir::new("config-symlink");
        fs::create_dir_all(temp.path().join(".rivet")).unwrap();
        let target = temp.path().join("real-config.toml");
        fs::write(&target, "[index]\n").unwrap();
        std::os::unix::fs::symlink(&target, temp.path().join(".rivet/config.toml")).unwrap();

        let error = Config::load(temp.path()).unwrap_err();
        assert!(matches!(error, ConfigError::Symlinked { .. }));
        assert!(error.to_string().contains("config.toml"));
    }
}
