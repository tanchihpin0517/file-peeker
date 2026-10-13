//! Which rows a filter leaves on screen.

use std::path::Path;

/// Which rows a frame may show: every row, or the rows a filter matched together
/// with the ancestors that keep them readable.
///
/// Filtering never removes rows. Expansion, streaming insertion, and collapse all
/// address rows by index, so a filter that dropped them would move every index out
/// from under an in-flight listing. It is a window instead, and the only state a
/// filter moves is the selection and the scroll offset -- both of which are
/// positions in this window rather than row indices.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum VisibleRows {
    All(usize),
    Filtered {
        rows: Vec<VisibleRow>,
        matches: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VisibleRow {
    pub(super) row: usize,
    /// False for a row on screen only to place a match underneath it.
    pub(super) matched: bool,
}

impl VisibleRows {
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::All(count) => *count,
            Self::Filtered { rows, .. } => rows.len(),
        }
    }

    /// How many rows matched outright, excluding the ancestors carried along for
    /// readability. `None` when every row is on screen anyway.
    pub(crate) fn matches(&self) -> Option<usize> {
        match self {
            Self::All(_) => None,
            Self::Filtered { matches, .. } => Some(*matches),
        }
    }

    pub(crate) fn row(&self, position: usize) -> Option<usize> {
        match self {
            Self::All(count) => (position < *count).then_some(position),
            Self::Filtered { rows, .. } => rows.get(position).map(|visible| visible.row),
        }
    }

    /// Where `row` sits, or where the nearest visible row after it sits, so a
    /// selection the filter just hid still lands somewhere sensible. `None` when
    /// there is nowhere to land at all.
    pub(crate) fn position_of(&self, row: usize) -> Option<usize> {
        let last = self.len().checked_sub(1)?;
        Some(match self {
            Self::All(_) => row.min(last),
            Self::Filtered { rows, .. } => start_of(rows, row).min(last),
        })
    }

    /// Whether `row` is on screen because it matched, rather than to place a match
    /// under it.
    pub(crate) fn matched(&self, row: usize) -> bool {
        match self {
            Self::All(_) => true,
            Self::Filtered { rows, .. } => rows
                .get(start_of(rows, row))
                .is_some_and(|visible| visible.row == row && visible.matched),
        }
    }

    /// Where the nearest match at or after `row` sits, falling back to the last
    /// match. This is where a filter puts the selection: an ancestor is context,
    /// not something the user asked to land on.
    ///
    /// A filtered list only exists because something matched, so a non-empty one
    /// always has a match to offer.
    pub(crate) fn match_position_of(&self, row: usize) -> Option<usize> {
        let Self::Filtered { rows, .. } = self else {
            return self.position_of(row);
        };
        let start = start_of(rows, row);
        rows.iter()
            .skip(start)
            .position(|visible| visible.matched)
            .map(|offset| start + offset)
            .or_else(|| rows.iter().rposition(|visible| visible.matched))
    }
}

/// The position `row` would take among visible rows, which is where it sits when
/// it is visible itself.
fn start_of(rows: &[VisibleRow], row: usize) -> usize {
    rows.partition_point(|visible| visible.row < row)
}

/// Whether a name matches a filter query, which is a case-insensitive substring.
///
/// Substring rather than a fuzzy subsequence, because completion has to have
/// something to complete to: a subsequence match shares no prefix with what was
/// typed, so `Tab` would have nothing to agree on.
///
/// The needle arrives already lowercased.
pub(crate) fn matches_needle(name: &str, needle: &str) -> bool {
    name.to_lowercase().contains(needle)
}

/// Whether a row lies inside the directory a `/` stepped into, or anywhere at all
/// when none has been.
///
/// Compared by path component rather than by string prefix, so entering `reports`
/// never captures `reports-old`.
pub(crate) fn in_scope(path: &str, scope: Option<&str>) -> bool {
    scope.is_none_or(|scope| Path::new(path).starts_with(scope))
}
