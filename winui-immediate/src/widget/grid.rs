use super::{Control, Response, must_write};
use crate::{
    bindings::{Grid, GridLength, GridUnitType, Panel, RowDefinition},
    pass::{ChildKind, ControlNode, Ui},
};
use std::hash::Hash;
use windows_core::{Interface, Result};

impl Ui {
    /// A grid with one row per entry of `rows`. The controls `body` declares
    /// fill the rows in order; a [`Length::Star`] row gives its control a
    /// definite height, so a list there fills it and scrolls.
    pub fn grid(
        &mut self,
        id: impl Hash,
        rows: &[Length],
        body: impl FnOnce(&mut Ui) -> Result<()>,
    ) -> Result<Response<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let grid = Grid::new()?;
            let children = grid.cast::<Panel>()?.Children()?;
            let definitions = grid.RowDefinitions()?;
            Ok(ControlNode::new(
                grid.cast()?,
                Control::Grid {
                    children,
                    definitions,
                    rows: Vec::new(),
                },
            ))
        })?;
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::Grid {
            definitions,
            rows: written,
            ..
        } = &mut control.control
        else {
            unreachable!("grid keys hold grids")
        };
        if must_write(stale, written.as_slice(), rows) {
            definitions.Clear()?;
            for row in rows {
                let definition = RowDefinition::new()?;
                definition.SetHeight(row.to_grid_length())?;
                definitions.Append(&definition)?;
            }
            written.clear();
            written.extend_from_slice(rows);
        }
        self.open(node);
        body(self)?;
        self.close()?;
        Ok(Response { ui: self, node })
    }
}

/// The height of a [`Ui::grid`] row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Length {
    /// As tall as its control wants.
    Auto,
    /// A fixed height in device-independent pixels.
    Pixels(f64),
    /// A share of the space left after the other rows, by weight.
    Star(f64),
}

impl Length {
    fn to_grid_length(self) -> GridLength {
        let (value, grid_unit_type) = match self {
            Length::Auto => (1.0, GridUnitType::Auto),
            Length::Pixels(value) => (value, GridUnitType::Pixel),
            Length::Star(weight) => (weight, GridUnitType::Star),
        };
        GridLength {
            value,
            grid_unit_type,
        }
    }
}
