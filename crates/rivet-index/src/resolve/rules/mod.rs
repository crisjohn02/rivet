//! One ordered resolution rule per file (T19 design).
//!
//! T19 provides [`imports`] and [`functions`]. T20 adds `receivers.rs` and T21
//! adds `new_expr.rs`; each is registered in `resolve/mod.rs` in that order.

pub(crate) mod functions;
pub(crate) mod imports;
