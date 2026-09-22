; PHP named-definition captures (T11).
;
; Capture conventions (docs/ADDING-A-LANGUAGE.md "Query capture conventions"):
;   @symbol.<kind>  the whole declaration node; its Tree-sitter range includes
;                   modifiers and is the recorded span.
;   @symbol.name    the declared short name.
;
; Property and constant declarations can declare several names in one
; declaration node, so their element lists are walked in Rust rather than
; encoded as a query (see `symbols` in mod.rs). Node names below were validated
; against the pinned `tree-sitter-php` grammar by the `php_extract` tests.

(namespace_definition
  name: (namespace_name) @symbol.name) @symbol.module

(class_declaration
  name: (name) @symbol.name) @symbol.class

(interface_declaration
  name: (name) @symbol.name) @symbol.interface

; Traits are class-kind in v0.1 (docs/ADDING-A-LANGUAGE.md "MVP support boundary").
(trait_declaration
  name: (name) @symbol.name) @symbol.class

(enum_declaration
  name: (name) @symbol.name) @symbol.enum

; Enum cases have no dedicated value in the contract's closed `kind` enum, and
; docs/ADDING-A-LANGUAGE.md "Named definitions" names constants but not enum
; cases. A PHP enum case is addressable and case-sensitive exactly like a class
; constant (`Suit::Hearts`), so it is recorded as `const` (T25).
(enum_case
  name: (name) @symbol.name) @symbol.const

(function_definition
  name: (name) @symbol.name) @symbol.function

(method_declaration
  name: (name) @symbol.name) @symbol.method

(property_declaration) @symbol.property

; A promoted constructor property is a real property declared in a parameter
; list. docs/ADDING-A-LANGUAGE.md "Named definitions" includes properties, so
; it is recorded as one (T25). Its name is a `variable_name`, keeping the `$`.
(property_promotion_parameter
  name: (variable_name) @symbol.name) @symbol.property

(const_declaration) @symbol.const
