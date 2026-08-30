//! Shared numbered diff facts consumed by the terminal painters.

use amux_ui::claude::DiffArtifact;

pub use super::claude::diff::reader_rows;
pub(crate) use amux_ui::diff::{RowFact as DiffRow, RowKind as DiffRowKind};

pub(crate) fn diff_rows_from_claude(artifact: &DiffArtifact) -> Vec<DiffRow> {
    artifact.document.rows()
}

pub(crate) fn diff_rows_from_patch(patch: &str, truncated: bool) -> Vec<DiffRow> {
    amux_ui::diff::parse_unified_patch(patch, truncated).rows()
}
