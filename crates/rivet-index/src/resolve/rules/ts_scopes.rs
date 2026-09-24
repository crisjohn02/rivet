//! TypeScript lexical scopes, relative module lookup, and module exports
//! (T44). Not a rule: the lookups [`ts_lexical`](super::ts_lexical) and
//! [`ts_namespace`](super::ts_namespace) share, as `rebinding.rs` is for the
//! PHP receiver rules.
//!
//! Everything here reads the persisted TypeScript scope facts (T43's `locals`
//! and `module_imports`, T44's `module_exports` and `value_type_uses`, and
//! `declares`) and the snapshot's file rows. Nothing reads the filesystem.
//!
//! - **Spaces.** A use looks its name up among values or among types
//!   ([`Space`]): a `type` use in a type position among types; every other
//!   use, and a `type` use its scope records as naming a value (`new C`,
//!   `instanceof C`, `typeof c`, a class's `extends C`), among values. An
//!   `import` use and the local name of an export (`export { a }`,
//!   `export default a`) name every meaning, so they need one declaration
//!   across both spaces ([`Space::Any`]).
//! - **Scope chain.** The nearest scope that binds the name in the use's space
//!   decides ([`binding_of`]): by its locals, or by its import bindings. A
//!   scope that binds it both ways is refused, and so is a name that a
//!   string-named ambient module body (`declare module "x" {}`) does not bind
//!   itself: the body also sees the exports of the module it declares or
//!   augments. A local names the symbol its
//!   scope `declares` under that name whose span holds the local's name
//!   ([`declaration_of_locals`]); a local that names no symbol (a parameter, a
//!   `let`, a function-local declaration, a global) binds nothing, so the use
//!   stays unresolved rather than reaching an outer declaration. The one
//!   exception is a folded overload signature, whose value local names the
//!   one function symbol of its name in that scope.
//! - **Merging.** More than one declaration in the needed space is refused
//!   (spec §11.4): two locals of one scope (`interface X` twice), or another
//!   declaration of the same qualified name elsewhere in the file (a second
//!   body of one namespace). Inside a namespace or enum declared more than
//!   once in its file, a body sees the other bodies' exported members, so a
//!   use there binds only a declaration inside its own body, and only when no
//!   other body has a member of that name ([`merged_body_conflict`]).
//! - **Modules.** A relative specifier (`./`, `../`) is joined to the
//!   importing file's directory and checked against the ordered candidate set
//!   of docs/ADDING-A-LANGUAGE.md: the exact path when it has a supported
//!   extension, then `.ts`, `.tsx`, `.d.ts`, `/index.ts`, `/index.tsx`,
//!   `/index.d.ts` ([`lookup_module`]). Candidates are compared byte for byte
//!   with the snapshot's file paths, never the filesystem, so `./Util` does
//!   not name `util.ts` on a case-insensitive disk. Exactly one candidate may
//!   be in the snapshot, and it must be an indexed TypeScript file; two
//!   candidates, or one that failed to index, leave the import unresolved.
//! - **Exports.** A module exports `name` as the declaration its module-scope
//!   local of the export's local name identifies ([`exported`]). A re-export
//!   of the name (`export { x as name } from`, `export * as name from`) is
//!   never followed, and neither is a name the module does not export locally,
//!   which only an `export *` could supply. An anonymous or expression
//!   default, and an exported import binding (`import { a } from "./x";
//!   export { a }`, a re-export in effect), identify no declaration.
//!
//! Circular imports cannot loop: resolving an import reads the imported
//! module's own scope facts and nothing it imports.

use std::collections::{HashMap, HashSet};

use rivet_core::extract::{
    BindingSpace, LocalBinding, ModuleExport, ModuleImport, ModuleImportKind,
};
use rivet_core::{ParseStatus, RefKind, Span, SymbolKind};
use rivet_store::{FileRow, SymbolRow, UseRow};

use crate::resolve::RuleCtx;

/// The key of a TypeScript file's module scope (T43's `MODULE_SCOPE_KEY`).
pub(crate) const MODULE_SCOPE: &str = "top:file";

/// The stored `files.language` of `.ts`, `.d.ts`, and `.tsx` files.
const TYPESCRIPT: &str = "typescript";

/// The candidate suffixes a relative specifier's path is tried with, after
/// the exact path, in docs/ADDING-A-LANGUAGE.md order.
const CANDIDATE_SUFFIXES: [&str; 6] = [
    ".ts",
    ".tsx",
    ".d.ts",
    "/index.ts",
    "/index.tsx",
    "/index.d.ts",
];

/// Which declarations a use's name can name (T43's value and type spaces).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Space {
    /// A value: a call, a read or write, a `new` target, `typeof x`.
    Value,
    /// A type: an annotation, a generic argument, `implements`.
    Type,
    /// Every meaning of the name: an `import` use, or the local name of an
    /// export. One declaration must cover them all.
    Any,
}

impl Space {
    /// Whether a local bound in `space` has a meaning this space looks up.
    fn admits(self, space: BindingSpace) -> bool {
        matches!(
            (self, space),
            (Space::Any, _)
                | (_, BindingSpace::Both)
                | (Space::Value, BindingSpace::Value)
                | (Space::Type, BindingSpace::Type)
        )
    }

    /// Whether a declaration of `kind` can have a meaning this space looks
    /// up: the decision's value kinds (function, class, enum, const,
    /// namespace) and type kinds (class, interface, enum, type alias,
    /// namespace). Only the merge check reads this, where no local gives the
    /// declaration's own space, so it errs toward refusing.
    fn admits_kind(self, kind: SymbolKind) -> bool {
        match self {
            Space::Any => true,
            Space::Value => matches!(
                kind,
                SymbolKind::Function
                    | SymbolKind::Class
                    | SymbolKind::Enum
                    | SymbolKind::Const
                    | SymbolKind::Module
            ),
            Space::Type => matches!(
                kind,
                SymbolKind::Class
                    | SymbolKind::Interface
                    | SymbolKind::Enum
                    | SymbolKind::TypeAlias
                    | SymbolKind::Module
            ),
        }
    }
}

/// One persisted TypeScript scope's facts, indexed by name.
pub(crate) struct TsScope<'a> {
    /// The enclosing scope's key, or `None` for the module scope.
    parent: Option<&'a str>,
    /// The names this scope binds, with their spaces, by name.
    locals: HashMap<String, Vec<LocalBinding>>,
    /// Import bindings and re-exports written in this scope.
    imports: Vec<ModuleImport>,
    /// Indices into [`imports`](Self::imports) of the bindings of each local
    /// name.
    imports_by_local: HashMap<String, Vec<usize>>,
    /// The names this scope's module re-exports (`export { x as name } from`,
    /// `export * as name from`), which are never followed.
    re_exported: HashSet<String>,
    /// What this scope's module exports from its own declarations, by
    /// exported name.
    exports: HashMap<String, Vec<ModuleExport>>,
    /// `(start_byte, end_byte)` of each export's local name as written.
    export_locals: HashSet<(u32, u32)>,
    /// The module-local symbols among the locals, by name.
    declared: HashMap<&'a str, Vec<&'a SymbolRow>>,
    /// The IDs of [`declared`](Self::declared).
    declared_ids: HashSet<&'a str>,
    /// `(start_byte, end_byte)` of each `type` use here that names a value.
    value_type_uses: HashSet<(u32, u32)>,
    /// Whether this is a string-named ambient module's body, which also sees
    /// the exports of the module it declares or augments.
    ambient_module: bool,
}

impl<'a> TsScope<'a> {
    /// Indexes one scope's persisted facts; `declared` are the symbols its
    /// `declares` lists.
    pub(crate) fn new(
        parent: Option<&'a str>,
        locals: Vec<LocalBinding>,
        imports: Vec<ModuleImport>,
        exports: Vec<ModuleExport>,
        declared: Vec<&'a SymbolRow>,
        value_type_uses: &[Span],
        ambient_module: bool,
    ) -> TsScope<'a> {
        let mut by_name: HashMap<String, Vec<LocalBinding>> = HashMap::new();
        for local in locals {
            by_name.entry(local.name.clone()).or_default().push(local);
        }
        let mut imports_by_local: HashMap<String, Vec<usize>> = HashMap::new();
        let mut re_exported = HashSet::new();
        for (index, import) in imports.iter().enumerate() {
            if let Some(local) = &import.local {
                imports_by_local
                    .entry(local.clone())
                    .or_default()
                    .push(index);
            }
            if matches!(
                import.kind,
                ModuleImportKind::ReExport | ModuleImportKind::ReExportAll
            ) && let Some(exported) = &import.exported
            {
                re_exported.insert(exported.clone());
            }
        }
        let export_locals = exports
            .iter()
            .filter(|export| export.local.is_some())
            .map(|export| (export.span.start_byte(), export.span.end_byte()))
            .collect();
        let mut by_exported: HashMap<String, Vec<ModuleExport>> = HashMap::new();
        for export in exports {
            by_exported
                .entry(export.exported.clone())
                .or_default()
                .push(export);
        }
        let declared_ids = declared.iter().map(|row| row.id.as_str()).collect();
        let mut declared_by_name: HashMap<&'a str, Vec<&'a SymbolRow>> = HashMap::new();
        for row in declared {
            declared_by_name
                .entry(row.name.as_str())
                .or_default()
                .push(row);
        }
        TsScope {
            parent,
            locals: by_name,
            imports,
            imports_by_local,
            re_exported,
            exports: by_exported,
            export_locals,
            declared: declared_by_name,
            declared_ids,
            value_type_uses: value_type_uses
                .iter()
                .map(|span| (span.start_byte(), span.end_byte()))
                .collect(),
            ambient_module,
        }
    }

    /// Import bindings and re-exports written in this scope.
    pub(crate) fn imports(&self) -> &[ModuleImport] {
        &self.imports
    }
}

/// The TypeScript view of one snapshot the TypeScript rules read (T44).
///
/// Empty in the PHP rule set's context.
#[derive(Default)]
pub(crate) struct TsModules<'a> {
    /// Every file of the snapshot by path, whatever its language or status:
    /// module lookup compares candidates with these paths.
    files: HashMap<&'a str, &'a FileRow>,
    /// Each TypeScript file's scopes by scope key.
    scopes: HashMap<&'a str, HashMap<&'a str, TsScope<'a>>>,
    /// Each TypeScript file's symbols by qualified name.
    by_qualified_name: HashMap<&'a str, HashMap<&'a str, Vec<&'a SymbolRow>>>,
    /// Each TypeScript file's namespace and enum declarations that share
    /// their qualified name with another namespace or enum declaration of
    /// the file (merged bodies).
    merged_bodies: HashMap<&'a str, Vec<&'a SymbolRow>>,
    /// Each TypeScript symbol's direct members, by parent ID.
    children: HashMap<&'a str, Vec<&'a SymbolRow>>,
}

impl<'a> TsModules<'a> {
    /// Indexes the snapshot's files and the TypeScript `symbols`; scopes are
    /// added by [`TsModules::insert_scope`].
    pub(crate) fn new(files: &'a [FileRow], symbols: &[&'a SymbolRow]) -> TsModules<'a> {
        let mut modules = TsModules {
            files: files.iter().map(|row| (row.path.as_str(), row)).collect(),
            ..TsModules::default()
        };
        for &row in symbols {
            modules
                .by_qualified_name
                .entry(row.file.as_str())
                .or_default()
                .entry(row.qualified_name.as_str())
                .or_default()
                .push(row);
            if let Some(parent) = row.parent_id.as_deref() {
                modules.children.entry(parent).or_default().push(row);
            }
        }
        for (&file, names) in &modules.by_qualified_name {
            for rows in names.values() {
                let bodies: Vec<&'a SymbolRow> = rows
                    .iter()
                    .copied()
                    .filter(|row| is_body(row.kind))
                    .collect();
                if bodies.len() > 1 {
                    modules
                        .merged_bodies
                        .entry(file)
                        .or_default()
                        .extend(bodies);
                }
            }
        }
        modules
    }

    /// Records one TypeScript scope of `file`.
    pub(crate) fn insert_scope(&mut self, file: &'a str, key: &'a str, scope: TsScope<'a>) {
        self.scopes.entry(file).or_default().insert(key, scope);
    }

    /// The scope `key` of `file`.
    pub(crate) fn scope(&self, file: &str, key: &str) -> Option<&TsScope<'a>> {
        self.scopes.get(file)?.get(key)
    }
}

/// The space `use_row`'s name is looked up in.
pub(crate) fn use_space(ctx: &RuleCtx<'_>, use_row: &UseRow) -> Space {
    let scope = ctx.typescript.scope(&use_row.file, &use_row.scope_key);
    let span = (use_row.start_byte, use_row.end_byte);
    match use_row.ref_kind {
        RefKind::Import => Space::Any,
        RefKind::Type => {
            if scope.is_some_and(|scope| scope.value_type_uses.contains(&span)) {
                Space::Value
            } else {
                Space::Type
            }
        }
        // `a` in `export { a as b }` and in `export default a` names every
        // meaning the export carries.
        RefKind::Unknown if scope.is_some_and(|scope| scope.export_locals.contains(&span)) => {
            Space::Any
        }
        _ => Space::Value,
    }
}

/// The nearest binding of a name on a scope chain.
pub(crate) enum Binding<'s, 'a> {
    /// No scope on the chain binds the name in the space: a global, or a
    /// name the file never declares.
    Unbound,
    /// The nearest binding scope binds it by these locals.
    Locals {
        /// That scope.
        scope: &'s TsScope<'a>,
        /// Its locals of the name in the space.
        locals: Vec<&'s LocalBinding>,
    },
    /// The nearest binding scope binds it by these import bindings.
    Imports {
        /// Whether that scope is the module scope, the only one whose
        /// imports are resolved.
        module_scope: bool,
        /// Its import bindings of the name.
        imports: Vec<&'s ModuleImport>,
    },
    /// The nearest binding scope binds it both by a local and by an import,
    /// or the chain is broken: never guess.
    Unknowable,
}

/// What `scope` alone says about `name` in `space`, or `None` when it does
/// not bind the name.
fn scope_binding<'s, 'a>(
    scope: &'s TsScope<'a>,
    key: &str,
    name: &str,
    space: Space,
) -> Option<Binding<'s, 'a>> {
    let locals: Vec<&LocalBinding> = scope
        .locals
        .get(name)
        .into_iter()
        .flatten()
        .filter(|local| space.admits(local.space))
        .collect();
    let imports: Vec<&ModuleImport> = scope
        .imports_by_local
        .get(name)
        .into_iter()
        .flatten()
        .map(|&index| &scope.imports[index])
        .collect();
    match (locals.is_empty(), imports.is_empty()) {
        (true, true) => None,
        (false, true) => Some(Binding::Locals { scope, locals }),
        (true, false) => Some(Binding::Imports {
            module_scope: key == MODULE_SCOPE,
            imports,
        }),
        (false, false) => Some(Binding::Unknowable),
    }
}

/// The nearest binding of `name` in `space`, starting at scope `key` of
/// `file` and walking parent scopes outward.
pub(crate) fn binding_of<'s, 'a>(
    ctx: &'s RuleCtx<'a>,
    file: &str,
    key: &str,
    name: &str,
    space: Space,
) -> Binding<'s, 'a> {
    let Some(scopes) = ctx.typescript.scopes.get(file) else {
        return Binding::Unknowable;
    };
    let mut current = Some(key);
    // A well-formed chain visits each scope at most once.
    let mut remaining = scopes.len();
    while let Some(key) = current {
        let Some(scope) = scopes.get(key) else {
            return Binding::Unknowable;
        };
        if let Some(binding) = scope_binding(scope, key, name, space) {
            return binding;
        }
        // An ambient module body also sees the exports of the module it
        // declares or augments, which may bind the name: never look past it.
        if scope.ambient_module || remaining == 0 {
            return Binding::Unknowable;
        }
        remaining -= 1;
        current = scope.parent;
    }
    Binding::Unbound
}

/// The one declaration the `locals` of `name` in `scope` of `file` identify
/// in `space`, or `None` when they name no symbol or more than one.
pub(crate) fn declaration_of_locals<'a>(
    ctx: &RuleCtx<'a>,
    file: &str,
    scope: &TsScope<'a>,
    locals: &[&LocalBinding],
    name: &str,
    space: Space,
) -> Option<&'a SymbolRow> {
    let declared: &[&'a SymbolRow] = scope.declared.get(name).map_or(&[], Vec::as_slice);
    let mut found: Option<&'a SymbolRow> = None;
    for local in locals {
        let holding: Vec<&'a SymbolRow> = declared
            .iter()
            .copied()
            .filter(|row| {
                row.start_byte <= local.span.start_byte() && local.span.end_byte() <= row.end_byte
            })
            .collect();
        let symbol = match holding.as_slice() {
            [row] => *row,
            // No symbol holds the name: a folded overload signature names the
            // one function of its name here; anything else (a parameter, a
            // `let`, a destructured name, a global) is no indexed declaration.
            [] => {
                let functions: Vec<&'a SymbolRow> = declared
                    .iter()
                    .copied()
                    .filter(|row| row.kind == SymbolKind::Function)
                    .collect();
                match (local.space, functions.as_slice()) {
                    (BindingSpace::Value, [row]) => *row,
                    _ => return None,
                }
            }
            _ => return None,
        };
        match found {
            None => found = Some(symbol),
            Some(existing) if existing.id == symbol.id => {}
            Some(_) => return None,
        }
    }
    let symbol = found?;
    // Another declaration of the same qualified name outside this scope (a
    // second body of one namespace) merges with this one.
    let merged = ctx
        .typescript
        .by_qualified_name
        .get(file)
        .and_then(|names| names.get(symbol.qualified_name.as_str()))
        .is_some_and(|rows| {
            rows.iter().any(|row| {
                row.id != symbol.id
                    && !scope.declared_ids.contains(row.id.as_str())
                    && space.admits_kind(row.kind)
            })
        });
    (!merged).then_some(symbol)
}

/// The one declaration the import bindings `imports` of the name give in
/// `space`, or `None`. Only a module scope's imports are resolved: a
/// relative import inside an ambient module body is not valid TypeScript.
pub(crate) fn declaration_of_imports<'a>(
    ctx: &RuleCtx<'a>,
    file: &str,
    module_scope: bool,
    imports: &[&ModuleImport],
    space: Space,
) -> Option<&'a SymbolRow> {
    if !module_scope {
        return None;
    }
    let mut found: Option<&'a SymbolRow> = None;
    for import in imports {
        let target = import_target(ctx, file, import, space)?;
        match found {
            None => found = Some(target),
            Some(existing) if existing.id == target.id => {}
            Some(_) => return None,
        }
    }
    found
}

/// The declaration one named or default import binding in `file` names in
/// `space`. A namespace import names a module, not a declaration, and a
/// `require` import is never resolved.
pub(crate) fn import_target<'a>(
    ctx: &RuleCtx<'a>,
    file: &str,
    import: &ModuleImport,
    space: Space,
) -> Option<&'a SymbolRow> {
    match import.kind {
        ModuleImportKind::Named | ModuleImportKind::Default => {
            let module = lookup_module(ctx, file, &import.specifier)?;
            exported(ctx, module, import.imported.as_deref()?, space)
        }
        ModuleImportKind::Namespace
        | ModuleImportKind::Require
        | ModuleImportKind::ReExport
        | ModuleImportKind::ReExportAll => None,
    }
}

/// The indexed TypeScript file a relative `specifier` written in `importer`
/// names, or `None` when it is not relative, leaves the repository, names no
/// candidate, names more than one, or names one that is not indexed.
pub(crate) fn lookup_module<'a>(
    ctx: &RuleCtx<'a>,
    importer: &str,
    specifier: &str,
) -> Option<&'a str> {
    if !(specifier.starts_with("./") || specifier.starts_with("../")) {
        return None;
    }
    let mut segments: Vec<&str> = importer.split('/').collect();
    segments.pop();
    for segment in specifier.split('/') {
        match segment {
            "." => {}
            ".." => {
                segments.pop()?;
            }
            // `.//x` and a trailing `/` have no candidate rule.
            "" => return None,
            other => segments.push(other),
        }
    }
    if segments.is_empty() {
        return None;
    }
    let base = segments.join("/");
    let mut candidates = Vec::with_capacity(CANDIDATE_SUFFIXES.len() + 1);
    if base.ends_with(".ts") || base.ends_with(".tsx") {
        candidates.push(base.clone());
    }
    candidates.extend(
        CANDIDATE_SUFFIXES
            .iter()
            .map(|suffix| format!("{base}{suffix}")),
    );
    let mut present = candidates
        .iter()
        .filter_map(|candidate| ctx.typescript.files.get_key_value(candidate.as_str()));
    let (&path, row) = present.next()?;
    if present.next().is_some() {
        return None;
    }
    (row.language.as_deref() == Some(TYPESCRIPT) && row.parse_status == ParseStatus::Ok)
        .then_some(path)
}

/// The one declaration `module` exports as `name` in `space`, or `None`.
pub(crate) fn exported<'a>(
    ctx: &RuleCtx<'a>,
    module: &str,
    name: &str,
    space: Space,
) -> Option<&'a SymbolRow> {
    let scope = ctx.typescript.scope(module, MODULE_SCOPE)?;
    // A re-export of the name is recorded but never followed.
    if scope.re_exported.contains(name) {
        return None;
    }
    // A name the module does not export locally (only an `export *` could
    // supply it) identifies nothing.
    let entries = scope.exports.get(name)?;
    let mut found: Option<&'a SymbolRow> = None;
    for export in entries {
        let local = export.local.as_deref()?;
        let symbol = match scope_binding(scope, MODULE_SCOPE, local, space)? {
            Binding::Locals { locals, .. } => {
                declaration_of_locals(ctx, module, scope, &locals, local, space)?
            }
            // An exported import binding is a re-export in effect.
            Binding::Imports { .. } | Binding::Unknowable | Binding::Unbound => return None,
        };
        match found {
            None => found = Some(symbol),
            Some(existing) if existing.id == symbol.id => {}
            Some(_) => return None,
        }
    }
    found
}

/// Whether binding `use_row` (whose name `name` was looked up lexically) to
/// `target` could ignore a merged declaration's member.
///
/// A namespace or enum body sees the exported members of every other body
/// of the same namespace or enum. So for each namespace or enum declaration
/// holding the use that is declared more than once in the file, the target
/// must lie inside that declaration, and no other declaration of it may have
/// a member named `name` (a member that is not a symbol, such as an
/// exported `let`, is not seen; the first condition already refuses every
/// target outside the use's own body).
pub(crate) fn merged_body_conflict(
    ctx: &RuleCtx<'_>,
    use_row: &UseRow,
    name: &str,
    target: &SymbolRow,
) -> bool {
    let Some(bodies) = ctx.typescript.merged_bodies.get(use_row.file.as_str()) else {
        return false;
    };
    bodies
        .iter()
        .filter(|row| row.start_byte <= use_row.start_byte && use_row.end_byte <= row.end_byte)
        .any(|declaration| {
            let others = bodies.iter().filter(|other| {
                other.id != declaration.id && other.qualified_name == declaration.qualified_name
            });
            let inside = target.file == declaration.file
                && declaration.start_byte <= target.start_byte
                && target.end_byte <= declaration.end_byte;
            !inside
                || others.into_iter().any(|other| {
                    ctx.typescript
                        .children
                        .get(other.id.as_str())
                        .is_some_and(|members| members.iter().any(|member| member.name == name))
                })
        })
}

/// Whether a declaration of `kind` has a body other declarations of its
/// name merge with: a namespace or an enum.
fn is_body(kind: SymbolKind) -> bool {
    matches!(kind, SymbolKind::Module | SymbolKind::Enum)
}

#[cfg(test)]
mod tests {
    use super::{TsModules, lookup_module};
    use crate::resolve::RuleCtx;
    use rivet_core::ParseStatus;
    use rivet_store::FileRow;

    fn file(path: &str, language: Option<&str>, parse_status: ParseStatus) -> FileRow {
        FileRow {
            path: path.to_string(),
            language: language.map(str::to_string),
            mtime_ns: 0,
            size: 0,
            content_hash: None,
            source: None,
            parse_status,
        }
    }

    fn ts(path: &str) -> FileRow {
        file(path, Some("typescript"), ParseStatus::Ok)
    }

    fn ctx(files: &[FileRow]) -> RuleCtx<'_> {
        let mut ctx = RuleCtx::new(Vec::new());
        ctx.typescript = TsModules::new(files, &[]);
        ctx
    }

    #[test]
    fn only_relative_specifiers_are_looked_up() {
        let files = [
            ts("src/util.ts"),
            ts("util.ts"),
            ts("node_modules/util/index.ts"),
        ];
        let ctx = ctx(&files);
        assert_eq!(
            lookup_module(&ctx, "src/app.ts", "./util"),
            Some("src/util.ts")
        );
        assert_eq!(
            lookup_module(&ctx, "src/app.ts", "../util"),
            Some("util.ts")
        );
        for specifier in ["util", "@/util", "/src/util", ".", "..", "", "src/util"] {
            assert_eq!(
                lookup_module(&ctx, "src/app.ts", specifier),
                None,
                "{specifier}"
            );
        }
    }

    #[test]
    fn each_candidate_of_the_ordered_set_is_found_and_must_be_unique() {
        for candidate in [
            "src/m.ts",
            "src/m.tsx",
            "src/m.d.ts",
            "src/m/index.ts",
            "src/m/index.tsx",
            "src/m/index.d.ts",
        ] {
            let files = [ts(candidate)];
            let found = lookup_module(&ctx(&files), "src/app.ts", "./m");
            assert_eq!(found, Some(candidate), "{candidate}");
        }
        // The exact path counts only with a supported extension.
        let files = [ts("src/m.ts")];
        assert_eq!(
            lookup_module(&ctx(&files), "src/app.ts", "./m.ts"),
            Some("src/m.ts")
        );
        let files = [file("src/m.js", None, ParseStatus::Unsupported)];
        assert_eq!(lookup_module(&ctx(&files), "src/app.ts", "./m.js"), None);
        // Any two candidates are ambiguous, whichever comes first.
        let pairs = [
            ("src/m.ts", "src/m.d.ts"),
            ("src/m.ts", "src/m.tsx"),
            ("src/m.ts", "src/m/index.ts"),
            ("src/m.d.ts", "src/m/index.d.ts"),
            ("src/m/index.ts", "src/m/index.tsx"),
        ];
        for (a, b) in pairs {
            let files = [ts(a), ts(b)];
            assert_eq!(
                lookup_module(&ctx(&files), "src/app.ts", "./m"),
                None,
                "{a} {b}"
            );
        }
    }

    #[test]
    fn a_candidate_must_be_an_indexed_typescript_file() {
        for status in [
            ParseStatus::ParseError,
            ParseStatus::ResourceLimit,
            ParseStatus::Size,
            ParseStatus::Encoding,
            ParseStatus::Binary,
        ] {
            let files = [file("src/m.ts", Some("typescript"), status)];
            assert_eq!(
                lookup_module(&ctx(&files), "src/app.ts", "./m"),
                None,
                "{status:?}"
            );
            // A failed candidate still counts toward ambiguity.
            let files = [
                file("src/m.ts", Some("typescript"), status),
                ts("src/m.d.ts"),
            ];
            assert_eq!(
                lookup_module(&ctx(&files), "src/app.ts", "./m"),
                None,
                "{status:?} beside a clean .d.ts"
            );
        }
        // A `.ts` path stored under another language (disabled TypeScript).
        let files = [file("src/m.ts", None, ParseStatus::Unsupported)];
        assert_eq!(lookup_module(&ctx(&files), "src/app.ts", "./m"), None);
    }

    #[test]
    fn specifier_paths_stay_inside_the_repository_and_match_bytes() {
        let files = [ts("a/b/c.ts"), ts("a/d.ts"), ts("e.ts")];
        let ctx = ctx(&files);
        assert_eq!(lookup_module(&ctx, "a/b/x.ts", "./c"), Some("a/b/c.ts"));
        assert_eq!(lookup_module(&ctx, "a/b/x.ts", "../d"), Some("a/d.ts"));
        assert_eq!(lookup_module(&ctx, "a/b/x.ts", "../../e"), Some("e.ts"));
        assert_eq!(
            lookup_module(&ctx, "a/b/x.ts", "./../b/./c"),
            Some("a/b/c.ts")
        );
        assert_eq!(lookup_module(&ctx, "a/b/x.ts", "../../../e"), None);
        assert_eq!(lookup_module(&ctx, "e.ts", "../e"), None);
        assert_eq!(lookup_module(&ctx, "a/b/x.ts", ".//c"), None);
        assert_eq!(lookup_module(&ctx, "a/b/x.ts", "./c/"), None);
        assert_eq!(lookup_module(&ctx, "a/b/x.ts", "./C"), None);
        assert_eq!(lookup_module(&ctx, "a/b/x.ts", "./c.TS"), None);
    }
}
