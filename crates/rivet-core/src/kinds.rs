//! Shared declaration, resolution, and reference-kind enums.
//!
//! The string forms match the output contract exactly; serialization will use
//! them once a JSON layer exists.

use std::fmt;
use std::str::FromStr;

/// The kind of a named declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SymbolKind {
    /// A class declaration.
    Class,
    /// A free function declaration.
    Function,
    /// A method declaration.
    Method,
    /// An interface declaration.
    Interface,
    /// A struct declaration.
    Struct,
    /// An enum declaration.
    Enum,
    /// A module or namespace declaration.
    Module,
    /// A property declaration.
    Property,
    /// A constant declaration.
    Const,
}

impl SymbolKind {
    /// The snake_case form used by the output contract.
    pub fn as_str(&self) -> &'static str {
        match self {
            SymbolKind::Class => "class",
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
            SymbolKind::Interface => "interface",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Module => "module",
            SymbolKind::Property => "property",
            SymbolKind::Const => "const",
        }
    }
}

impl FromStr for SymbolKind {
    type Err = KindParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "class" => Ok(SymbolKind::Class),
            "function" => Ok(SymbolKind::Function),
            "method" => Ok(SymbolKind::Method),
            "interface" => Ok(SymbolKind::Interface),
            "struct" => Ok(SymbolKind::Struct),
            "enum" => Ok(SymbolKind::Enum),
            "module" => Ok(SymbolKind::Module),
            "property" => Ok(SymbolKind::Property),
            "const" => Ok(SymbolKind::Const),
            _ => Err(KindParseError::new("symbol_kind", value)),
        }
    }
}

impl fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Evidence tier for a declaration link, ordered strongest to weakest.
///
/// The derived ordering places `Exact < Scoped < NameMatch`, so a resolution
/// "meets" a minimum when it is greater than or equal to that minimum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Resolution {
    /// A supported lexical binding identifies one declaration.
    Exact,
    /// A visible receiver/type/namespace heuristic identifies one declaration.
    Scoped,
    /// Spelling alone is evidence, or the binding is unsupported/ambiguous.
    NameMatch,
}

impl Resolution {
    /// The snake_case form used by the output contract.
    pub fn as_str(&self) -> &'static str {
        match self {
            Resolution::Exact => "exact",
            Resolution::Scoped => "scoped",
            Resolution::NameMatch => "name_match",
        }
    }

    /// Returns true when `self` is strong enough to satisfy `minimum`.
    ///
    /// The derived order runs from strongest (`Exact`) to weakest
    /// (`NameMatch`), so filtering with `minimum = Scoped` retains exact and
    /// scoped results and drops name matches.
    pub fn min_resolution(&self, minimum: Resolution) -> bool {
        *self <= minimum
    }
}

impl FromStr for Resolution {
    type Err = KindParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "exact" => Ok(Resolution::Exact),
            "scoped" => Ok(Resolution::Scoped),
            "name_match" => Ok(Resolution::NameMatch),
            _ => Err(KindParseError::new("resolution", value)),
        }
    }
}

impl fmt::Display for Resolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The syntactic form of a reference use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RefKind {
    /// A call expression.
    Call,
    /// A type annotation or reference.
    Type,
    /// An import or use statement.
    Import,
    /// An assignment target.
    Assignment,
    /// A read of a value.
    Read,
    /// A write to a value.
    Write,
    /// A use whose finer classification is unsupported.
    Unknown,
}

impl RefKind {
    /// The snake_case form used by the output contract.
    pub fn as_str(&self) -> &'static str {
        match self {
            RefKind::Call => "call",
            RefKind::Type => "type",
            RefKind::Import => "import",
            RefKind::Assignment => "assignment",
            RefKind::Read => "read",
            RefKind::Write => "write",
            RefKind::Unknown => "unknown",
        }
    }
}

impl FromStr for RefKind {
    type Err = KindParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "call" => Ok(RefKind::Call),
            "type" => Ok(RefKind::Type),
            "import" => Ok(RefKind::Import),
            "assignment" => Ok(RefKind::Assignment),
            "read" => Ok(RefKind::Read),
            "write" => Ok(RefKind::Write),
            "unknown" => Ok(RefKind::Unknown),
            _ => Err(KindParseError::new("ref_kind", value)),
        }
    }
}

impl fmt::Display for RefKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when an enum string is not one of its contract values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindParseError {
    enum_name: &'static str,
    value: String,
}

impl KindParseError {
    fn new(enum_name: &'static str, value: &str) -> KindParseError {
        KindParseError {
            enum_name,
            value: value.to_string(),
        }
    }

    /// The name of the enum that failed to parse.
    pub fn enum_name(&self) -> &'static str {
        self.enum_name
    }

    /// The rejected input string.
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for KindParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unrecognized {} value: {:?}", self.enum_name, self.value)
    }
}

impl std::error::Error for KindParseError {}

#[cfg(test)]
mod tests {
    use super::{KindParseError, RefKind, Resolution, SymbolKind};
    use std::str::FromStr;

    #[test]
    fn symbol_kind_strings_round_trip() {
        let kinds = [
            SymbolKind::Class,
            SymbolKind::Function,
            SymbolKind::Method,
            SymbolKind::Interface,
            SymbolKind::Struct,
            SymbolKind::Enum,
            SymbolKind::Module,
            SymbolKind::Property,
            SymbolKind::Const,
        ];
        let expected = [
            "class",
            "function",
            "method",
            "interface",
            "struct",
            "enum",
            "module",
            "property",
            "const",
        ];
        for (kind, text) in kinds.iter().zip(expected) {
            assert_eq!(kind.as_str(), text);
            assert_eq!(SymbolKind::from_str(text).unwrap(), *kind);
        }
        assert_eq!(
            SymbolKind::from_str("namespace").unwrap_err(),
            KindParseError::new("symbol_kind", "namespace")
        );
    }

    #[test]
    fn ref_kind_strings_round_trip() {
        let kinds = [
            RefKind::Call,
            RefKind::Type,
            RefKind::Import,
            RefKind::Assignment,
            RefKind::Read,
            RefKind::Write,
            RefKind::Unknown,
        ];
        let expected = [
            "call",
            "type",
            "import",
            "assignment",
            "read",
            "write",
            "unknown",
        ];
        for (kind, text) in kinds.iter().zip(expected) {
            assert_eq!(kind.as_str(), text);
            assert_eq!(RefKind::from_str(text).unwrap(), *kind);
        }
        assert!(RefKind::from_str("").is_err());
    }

    #[test]
    fn resolution_ordering_and_minimum_filter() {
        assert!(Resolution::Exact < Resolution::Scoped);
        assert!(Resolution::Scoped < Resolution::NameMatch);
        assert!(Resolution::Exact < Resolution::NameMatch);

        // exact and scoped meet a scoped minimum; name_match does not.
        assert!(Resolution::Exact.min_resolution(Resolution::Scoped));
        assert!(Resolution::Scoped.min_resolution(Resolution::Scoped));
        assert!(!Resolution::NameMatch.min_resolution(Resolution::Scoped));

        assert!(Resolution::NameMatch.min_resolution(Resolution::NameMatch));
        assert!(Resolution::Exact.min_resolution(Resolution::NameMatch));

        assert!("exact".parse::<Resolution>().unwrap() == Resolution::Exact);
        assert_eq!(
            Resolution::NameMatch.as_str(),
            "name_match",
            "contract spelling"
        );
        assert!(Resolution::from_str("EXACT").is_err());
    }
}
