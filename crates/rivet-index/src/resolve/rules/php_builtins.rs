//! PHP builtins with by-reference parameters (T21b).
//!
//! Builtins have no indexed declaration, so the resolver cannot read a
//! parameter list for them the way it does for a project function. This table
//! names the builtins whose parameter at a given 0-based position is declared
//! by reference; a variable passed at such a position is rebound and cannot be
//! trusted as a `new`-receiver.
//!
//! The table is deliberately small and fails safe. It only needs to be complete
//! enough to avoid claiming a by-value call is safe: an unknown global function
//! is suppressed, not assumed safe, so an omission never produces a wrong
//! binding. Add an entry whenever a builtin used by the project declares a
//! parameter by reference, listing every by-reference positional index.
//! Positions not listed are by-value, so passing the variable elsewhere in the
//! same builtin keeps its binding (for example `str_replace`'s count is its
//! fourth parameter, position 3).

/// `(lowercase builtin name, by-reference 0-based parameter positions)`.
pub(crate) const PHP_BY_REF_BUILTINS: &[(&str, &[u32])] = &[
    ("array_pop", &[0]),
    ("array_push", &[0]),
    ("array_shift", &[0]),
    ("array_splice", &[0]),
    ("array_unshift", &[0]),
    ("array_walk", &[0]),
    ("arsort", &[0]),
    ("asort", &[0]),
    ("krsort", &[0]),
    ("ksort", &[0]),
    ("natcasesort", &[0]),
    ("natsort", &[0]),
    ("parse_str", &[1]),
    ("preg_match", &[2]),
    ("preg_match_all", &[2]),
    ("rsort", &[0]),
    ("settype", &[0]),
    ("shuffle", &[0]),
    ("similar_text", &[2]),
    ("sort", &[0]),
    ("str_ireplace", &[3]),
    ("str_replace", &[3]),
    ("uasort", &[0]),
    ("uksort", &[0]),
    ("usort", &[0]),
];

/// The by-reference positions of a known builtin, or `None` when it is not in
/// the table.
///
/// `None` means the caller must suppress rather than assume by-value.
pub(crate) fn by_ref_positions(name: &str) -> Option<&'static [u32]> {
    let folded = name.to_ascii_lowercase();
    PHP_BY_REF_BUILTINS
        .iter()
        .find(|(builtin, _)| *builtin == folded)
        .map(|(_, positions)| *positions)
}

#[cfg(test)]
mod tests {
    use super::by_ref_positions;

    #[test]
    fn known_builtins_report_their_by_reference_positions() {
        assert_eq!(by_ref_positions("preg_match"), Some([2].as_slice()));
        assert_eq!(by_ref_positions("PREG_MATCH"), Some([2].as_slice()));
        assert_eq!(by_ref_positions("str_replace"), Some([3].as_slice()));
        assert_eq!(by_ref_positions("sort"), Some([0].as_slice()));
    }

    #[test]
    fn unknown_builtins_report_none() {
        assert_eq!(by_ref_positions("strlen"), None);
        assert_eq!(by_ref_positions("array_map"), None);
    }
}
