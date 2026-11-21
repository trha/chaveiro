//! Which constraints place a grid's children, free of AppKit types.

use super::Length;

/// The layout rules for the first `children` rows of `rows`, one child per
/// row in order. Rows past the last child take no space. Every child is also
/// pinned to the grid's leading and trailing edges, which needs no rule.
///
/// Panics when there are more children than rows.
pub(crate) fn rules(rows: &[Length], children: usize) -> Vec<Rule> {
    assert!(children <= rows.len(), "a grid has a row for every child");
    let rows = &rows[..children];
    let mut rules = Vec::new();
    // The first `Star` row with a positive weight is the unit that the other
    // `Star` rows share against; a zero weight cannot be divided by.
    let unit = rows
        .iter()
        .position(|row| matches!(row, Length::Star(weight) if *weight > 0.0));
    for (row, length) in rows.iter().enumerate() {
        rules.push(Rule::Top { row });
        match *length {
            Length::Auto => {}
            Length::Pixels(points) => rules.push(Rule::Height { row, points }),
            Length::Star(weight) => {
                rules.push(Rule::Stretch { row });
                match unit {
                    Some(unit) if unit == row => {}
                    Some(unit) => {
                        let Length::Star(unit_weight) = rows[unit] else {
                            unreachable!("the unit is a star row")
                        };
                        rules.push(Rule::Share {
                            row,
                            unit,
                            ratio: weight / unit_weight,
                        });
                    }
                    None => rules.push(Rule::Height { row, points: 0.0 }),
                }
            }
        }
    }
    if let Some(row) = children.checked_sub(1) {
        rules.push(Rule::Bottom {
            row,
            exact: rows.iter().any(|row| matches!(row, Length::Star(_))),
        });
    }
    rules
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Rule {
    /// The child's top edge sits on the grid's top edge (row 0) or on the
    /// bottom edge of the row above.
    Top {
        row: usize,
    },
    /// The last child's bottom edge sits on the grid's bottom edge when
    /// `exact`, so `Star` rows take the space left. Otherwise it stays at or
    /// above it, and the grid hugs its content.
    Bottom {
        row: usize,
        exact: bool,
    },
    Height {
        row: usize,
        points: f64,
    },
    /// `height(row) = ratio * height(unit)`, where `unit` is the first `Star`
    /// row with a positive weight.
    Share {
        row: usize,
        unit: usize,
        ratio: f64,
    },
    /// The child of a `Star` row gives up its vertical hugging, so it fills
    /// the row instead of pulling the row, and with it the window, down to
    /// its natural height.
    Stretch {
        row: usize,
    },
}

#[cfg(test)]
mod tests;
