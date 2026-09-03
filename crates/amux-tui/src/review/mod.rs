//! The review page: the terminal's visual layer over `amux_ui::review`.
//!
//! Every fact about rows, anchors, magnitudes and comments comes from the
//! review core; this module owns only what a screen owns — width, scroll,
//! cursor, folds and the file overlay. A side pane instead of a fullscreen
//! page would be a change here alone.

pub mod comments;
#[cfg(any(test, feature = "fixtures"))]
pub mod fixture;
pub mod reader;
pub mod view;

pub use comments::{CommentEditor, Selection};
pub use reader::review_reader_rows;
pub use view::{ReviewOutcome, ReviewView};
