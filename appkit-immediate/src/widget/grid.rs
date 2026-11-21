use super::{Control, Response, must_write};
use crate::pass::{ChildKind, ControlNode, Ui};
use objc2::rc::Retained;
use objc2_app_kit::{NSLayoutConstraint, NSView};
use std::hash::Hash;

mod rules;

pub(crate) use rules::{Rule, rules};

impl Ui {
    /// A grid with one row per entry of `rows`. The controls `body` declares
    /// fill the rows in order, each as wide as the grid; rows past the last
    /// control take no space. A [`Length::Star`] row takes a share of the
    /// space the other rows leave, so a table view there fills it and
    /// scrolls. A grid with no star row is as tall as its content.
    ///
    /// Panics when `body` declares more controls than `rows` has entries.
    pub fn grid(
        &mut self,
        id: impl Hash,
        rows: &[Length],
        body: impl FnOnce(&mut Ui),
    ) -> Response<'_> {
        let (node, _) = self.visit(id, ChildKind::Visual, |mtm| {
            let container = NSView::new(mtm);
            ControlNode::payload(
                container.clone(),
                Control::Grid {
                    container,
                    rows: Vec::new(),
                    constraints: Vec::new(),
                    placed: false,
                    stretched: Vec::new(),
                },
            )
        });
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::Grid {
            rows: written,
            placed,
            ..
        } = &mut control.control
        else {
            unreachable!("grid keys hold grids")
        };
        if must_write(node, stale, written.as_slice(), rows) {
            written.clear();
            written.extend_from_slice(rows);
            *placed = false;
        }
        self.open(node);
        body(self);
        self.close();
        Response { ui: self, node }
    }
}

/// The height of a [`Ui::grid`] row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Length {
    /// As tall as its control wants.
    Auto,
    /// A fixed height in points.
    Pixels(f64),
    /// A share of the space left after the other rows, by weight.
    Star(f64),
}

/// The constraints that place `children` in `container` by `rules`: the
/// row rules, and each child pinned to the leading and trailing edges.
pub(crate) fn constraints(
    container: &NSView,
    children: &[Retained<NSView>],
    rules: &[Rule],
) -> Vec<Retained<NSLayoutConstraint>> {
    let mut constraints = Vec::new();
    for child in children {
        constraints.push(
            child
                .leadingAnchor()
                .constraintEqualToAnchor(&container.leadingAnchor()),
        );
        constraints.push(
            child
                .trailingAnchor()
                .constraintEqualToAnchor(&container.trailingAnchor()),
        );
    }
    for rule in rules {
        match *rule {
            Rule::Top { row: 0 } => constraints.push(
                children[0]
                    .topAnchor()
                    .constraintEqualToAnchor(&container.topAnchor()),
            ),
            Rule::Top { row } => constraints.push(
                children[row]
                    .topAnchor()
                    .constraintEqualToAnchor(&children[row - 1].bottomAnchor()),
            ),
            Rule::Bottom { row, exact: true } => constraints.push(
                children[row]
                    .bottomAnchor()
                    .constraintEqualToAnchor(&container.bottomAnchor()),
            ),
            Rule::Bottom { row, exact: false } => {
                constraints.push(
                    children[row]
                        .bottomAnchor()
                        .constraintLessThanOrEqualToAnchor(&container.bottomAnchor()),
                );
                // Weaker than any content hugging, so extra space stays
                // below the last row, while the grid's fitting size still
                // ends at it.
                let hug = children[row]
                    .bottomAnchor()
                    .constraintEqualToAnchor(&container.bottomAnchor());
                hug.setPriority(1.0);
                constraints.push(hug);
            }
            Rule::Height { row, points } => constraints.push(
                children[row]
                    .heightAnchor()
                    .constraintEqualToConstant(points),
            ),
            // The caller lowers the child's hugging; it is not a constraint.
            Rule::Stretch { .. } => {}
            Rule::Share { row, unit, ratio } => constraints.push(
                children[row]
                    .heightAnchor()
                    .constraintEqualToAnchor_multiplier(&children[unit].heightAnchor(), ratio),
            ),
        }
    }
    constraints
}
