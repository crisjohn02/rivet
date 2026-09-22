//! Canonical symbol IDs.
//!
//! A canonical ID is `<repo-relative path>#<qualified name>`, optionally
//! followed by `#<one-based ordinal>` for duplicate declarations within one
//! file. Literal `%` and `#` inside either component are escaped as `%25` and
//! `%23` before joining, so the canonical form can be split on `#` and decoded
//! losslessly.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use crate::kinds::SymbolKind;
use crate::span::Span;

/// A parsed canonical symbol ID.
///
/// Paths and qualified names are stored decoded; the escaped form is produced
/// on demand by [`fmt::Display`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SymbolId {
    path: String,
    qualified_name: String,
    ordinal: Option<u32>,
}

impl SymbolId {
    /// Builds an ID from its decoded components.
    ///
    /// Empty path/qualified-name components and a zero ordinal are rejected so
    /// that every constructed ID round-trips through [`SymbolId::parse`].
    pub fn new(
        path: impl Into<String>,
        qualified_name: impl Into<String>,
        ordinal: Option<u32>,
    ) -> Result<SymbolId, SymbolIdError> {
        let path = path.into();
        let qualified_name = qualified_name.into();
        if path.is_empty() {
            return Err(SymbolIdError::EmptyComponent { component: "path" });
        }
        if qualified_name.is_empty() {
            return Err(SymbolIdError::EmptyComponent {
                component: "qualified_name",
            });
        }
        if matches!(ordinal, Some(0)) {
            return Err(SymbolIdError::ZeroOrdinal);
        }
        Ok(SymbolId {
            path,
            qualified_name,
            ordinal,
        })
    }

    /// Parses a canonical ID string.
    ///
    /// Splits on `#`, decodes both components, and reads an optional trailing
    /// one-based ordinal. Malformed escapes, empty components, zero ordinals,
    /// and additional trailing components are rejected.
    pub fn parse(input: &str) -> Result<SymbolId, SymbolIdError> {
        let mut parts = input.split('#');

        let path = parts.next().unwrap_or_default();
        let qualified = parts.next().ok_or(SymbolIdError::MissingSeparator)?;

        let path = decode_component(path)?;
        let qualified = decode_component(qualified)?;
        if path.is_empty() {
            return Err(SymbolIdError::EmptyComponent { component: "path" });
        }
        if qualified.is_empty() {
            return Err(SymbolIdError::EmptyComponent {
                component: "qualified_name",
            });
        }

        let ordinal = match parts.next() {
            None => None,
            Some(raw) => {
                if parts.next().is_some() {
                    return Err(SymbolIdError::TooManyComponents);
                }
                let value: u32 = raw.parse().map_err(|_| SymbolIdError::BadOrdinal {
                    value: raw.to_string(),
                })?;
                if value == 0 {
                    return Err(SymbolIdError::ZeroOrdinal);
                }
                Some(value)
            }
        };

        Ok(SymbolId {
            path,
            qualified_name: qualified,
            ordinal,
        })
    }

    /// The decoded repository-relative path.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The decoded language-native qualified name.
    pub fn qualified_name(&self) -> &str {
        &self.qualified_name
    }

    /// The one-based duplicate ordinal, if present.
    pub fn ordinal(&self) -> Option<u32> {
        self.ordinal
    }

    /// The escaped canonical string.
    pub fn as_canonical(&self) -> String {
        let mut out = encode_component(&self.path);
        out.push('#');
        out.push_str(&encode_component(&self.qualified_name));
        if let Some(ordinal) = self.ordinal {
            out.push('#');
            out.push_str(&ordinal.to_string());
        }
        out
    }
}

impl fmt::Display for SymbolId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_canonical())
    }
}

impl FromStr for SymbolId {
    type Err = SymbolIdError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        SymbolId::parse(input)
    }
}

/// Escapes literal `%` and `#` in one ID component.
fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '%' => out.push_str("%25"),
            '#' => out.push_str("%23"),
            _ => out.push(character),
        }
    }
    out
}

/// Decodes `%25` and `%23`, rejecting any other `%` sequence.
fn decode_component(value: &str) -> Result<String, SymbolIdError> {
    let mut out = String::with_capacity(value.len());
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '%' {
            out.push(character);
            continue;
        }
        let high = characters.next().ok_or_else(|| SymbolIdError::BadEscape {
            value: value.to_string(),
        })?;
        let low = characters.next().ok_or_else(|| SymbolIdError::BadEscape {
            value: value.to_string(),
        })?;
        match (high, low) {
            ('2', '5') => out.push('%'),
            ('2', '3') => out.push('#'),
            _ => {
                return Err(SymbolIdError::BadEscape {
                    value: value.to_string(),
                });
            }
        }
    }
    Ok(out)
}

/// Assigns one-based duplicate ordinals for declarations within one file.
///
/// `items` holds `(qualified_name, span, kind)` in any order. Names that occur
/// more than once are numbered `1..n` in `(start_byte, end_byte, kind)` order;
/// every member of a duplicate group receives an ordinal, including `#1`.
/// Unique names receive `None`. The result follows the input order.
pub fn assign_ordinals<S: AsRef<str>>(items: &[(S, Span, SymbolKind)]) -> Vec<Option<u32>> {
    let mut result = vec![None; items.len()];

    let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, (name, _, _)) in items.iter().enumerate() {
        groups.entry(name.as_ref()).or_default().push(index);
    }

    for indices in groups.values() {
        if indices.len() < 2 {
            continue;
        }
        let mut ordered = indices.clone();
        ordered.sort_by_key(|&index| {
            let (_, span, kind) = &items[index];
            (span.start_byte(), span.end_byte(), *kind)
        });
        for (rank, &index) in ordered.iter().enumerate() {
            result[index] = Some(rank as u32 + 1);
        }
    }

    result
}

/// Error returned when a canonical ID cannot be built or parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolIdError {
    /// A path or qualified-name component was empty.
    EmptyComponent {
        /// Which component was empty.
        component: &'static str,
    },
    /// The input had no `#` separating path and qualified name.
    MissingSeparator,
    /// A `%` escape was incomplete or unknown.
    BadEscape {
        /// The component containing the bad escape.
        value: String,
    },
    /// The trailing ordinal was not a number.
    BadOrdinal {
        /// The rejected ordinal text.
        value: String,
    },
    /// The trailing ordinal was zero.
    ZeroOrdinal,
    /// More than one trailing ordinal-like component was present.
    TooManyComponents,
}

impl fmt::Display for SymbolIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SymbolIdError::EmptyComponent { component } => {
                write!(f, "symbol id has an empty {component} component")
            }
            SymbolIdError::MissingSeparator => {
                f.write_str("symbol id is missing the '#' separator")
            }
            SymbolIdError::BadEscape { value } => {
                write!(f, "symbol id has a bad escape in component {value:?}")
            }
            SymbolIdError::BadOrdinal { value } => {
                write!(f, "symbol id has a non-numeric ordinal {value:?}")
            }
            SymbolIdError::ZeroOrdinal => f.write_str("symbol id ordinal must be one-based"),
            SymbolIdError::TooManyComponents => {
                f.write_str("symbol id has more than one trailing ordinal")
            }
        }
    }
}

impl std::error::Error for SymbolIdError {}

#[cfg(test)]
mod tests {
    use super::{SymbolId, assign_ordinals};
    use crate::kinds::SymbolKind;
    use crate::span::Span;

    #[test]
    fn round_trips_escaped_path_and_ordinal() {
        let id = SymbolId::new("src/a#b%c.rs", "Foo.bar", Some(2)).unwrap();
        assert_eq!(id.as_canonical(), "src/a%23b%25c.rs#Foo.bar#2");
        assert_eq!(SymbolId::parse(&id.to_string()).unwrap(), id);
    }

    #[test]
    fn round_trips_php_qualified_name() {
        let id = SymbolId::new(
            "app/Services/SurveyService.php",
            "App\\Services\\SurveyService::launch",
            None,
        )
        .unwrap();
        assert_eq!(
            id.as_canonical(),
            "app/Services/SurveyService.php#App\\Services\\SurveyService::launch"
        );
        assert_eq!(SymbolId::parse(id.as_canonical().as_str()).unwrap(), id);
        assert_eq!(id.qualified_name(), "App\\Services\\SurveyService::launch");
    }

    #[test]
    fn round_trips_literal_percent_escape_text() {
        // The name contains the five characters "%23", not a '#'.
        let id = SymbolId::new("src/x.rs", "weird%23name", None).unwrap();
        assert_eq!(id.as_canonical(), "src/x.rs#weird%2523name");
        let parsed = SymbolId::parse(id.as_canonical().as_str()).unwrap();
        assert_eq!(parsed.qualified_name(), "weird%23name");
        assert_eq!(parsed, id);
    }

    #[test]
    fn parse_rejects_malformed_ids() {
        for input in [
            "a#b#0",   // zero ordinal
            "a##b",    // empty qualified name
            "a%2#b",   // incomplete escape
            "#b",      // empty path
            "a#",      // empty qualified name
            "a#b#1#2", // more than one trailing ordinal
            "nohash",  // missing separator
            "a#b#x",   // non-numeric ordinal
        ] {
            assert!(
                SymbolId::parse(input).is_err(),
                "expected rejection of {input:?}"
            );
        }
        assert!(SymbolId::new("", "name", None).is_err());
        assert!(SymbolId::new("path", "", None).is_err());
        assert!(SymbolId::new("path", "name", Some(0)).is_err());
    }

    #[test]
    fn assigns_ordinals_in_span_order() {
        let foo = "App\\Survey::run";
        let items = vec![
            (foo, Span::new(10, 20).unwrap(), SymbolKind::Method),
            (foo, Span::new(30, 45).unwrap(), SymbolKind::Method),
            (foo, Span::new(0, 5).unwrap(), SymbolKind::Method),
            (
                "App\\Survey::unique",
                Span::new(50, 60).unwrap(),
                SymbolKind::Method,
            ),
        ];
        let ordinals = assign_ordinals(&items);
        assert_eq!(ordinals, vec![Some(2), Some(3), Some(1), None]);
    }

    #[test]
    fn ordinal_ties_break_on_kind_and_include_first_member() {
        let same_span = Span::new(4, 8).unwrap();
        let items = vec![
            ("dup", same_span, SymbolKind::Method),
            ("dup", same_span, SymbolKind::Function),
            ("other", Span::new(0, 3).unwrap(), SymbolKind::Class),
        ];
        // SymbolKind order is Class < Function < Method, so Function is #1.
        assert_eq!(assign_ordinals(&items), vec![Some(2), Some(1), None]);
    }
}
