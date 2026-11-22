use super::{Control, Response, must_write};
use crate::{
    bindings::{ColumnDefinition, Grid, GridLength, GridUnitType, Panel, RowDefinition},
    pass::{ChildKind, ControlNode, Ui},
    tree::NodeId,
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
        let node = self.grid_node(id, false, rows, |_| Ok(()), body)?;
        Ok(Response { ui: self, node })
    }

    /// A grid of one row with one column per entry of `columns`, `spacing`
    /// apart. The controls `body` declares fill the columns in order; a
    /// [`Length::Star`] column gives its control a definite width, so the
    /// control can stretch or wrap in it.
    pub fn columns(
        &mut self,
        id: impl Hash,
        columns: &[Length],
        spacing: f64,
        body: impl FnOnce(&mut Ui) -> Result<()>,
    ) -> Result<Response<'_>> {
        let node = self.grid_node(id, true, columns, |_| Ok(()), body)?;
        self.set_column_spacing(node, spacing)?;
        Ok(Response { ui: self, node })
    }

    /// A grid of one cell: the controls `body` declares all fill it, each
    /// drawn over the ones before, such as a progress bar over the content
    /// it belongs to. Give an overlaid control an alignment, so it covers
    /// only its part.
    pub fn overlay(
        &mut self,
        id: impl Hash,
        body: impl FnOnce(&mut Ui) -> Result<()>,
    ) -> Result<Response<'_>> {
        // A grid with no rows puts each child, whatever row it is given,
        // in its one implicit row.
        let node = self.grid_node(id, false, &[], |_| Ok(()), body)?;
        Ok(Response { ui: self, node })
    }

    /// Visits a grid whose children fill its columns when `across`, or its
    /// rows. `create` runs once, on the new grid.
    pub(crate) fn grid_node(
        &mut self,
        id: impl Hash,
        across: bool,
        lengths: &[Length],
        create: impl FnOnce(&Grid) -> Result<()>,
        body: impl FnOnce(&mut Ui) -> Result<()>,
    ) -> Result<NodeId> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let grid = Grid::new()?;
            create(&grid)?;
            let children = grid.cast::<Panel>()?.Children()?;
            Ok(ControlNode::new(
                grid.cast()?,
                Control::Grid {
                    grid,
                    children,
                    across,
                    lengths: Vec::new(),
                    spacing: f64::NAN,
                },
            ))
        })?;
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::Grid {
            grid,
            across: written_across,
            lengths: written,
            ..
        } = &mut control.control
        else {
            unreachable!("grid keys hold grids")
        };
        assert_eq!(
            *written_across, across,
            "a grid keeps its direction under one id"
        );
        if must_write(stale, written.as_slice(), lengths) {
            match across {
                true => {
                    let definitions = grid.ColumnDefinitions()?;
                    definitions.Clear()?;
                    for length in lengths {
                        let definition = ColumnDefinition::new()?;
                        definition.SetWidth(length.to_grid_length())?;
                        definitions.Append(&definition)?;
                    }
                }
                false => {
                    let definitions = grid.RowDefinitions()?;
                    definitions.Clear()?;
                    for length in lengths {
                        let definition = RowDefinition::new()?;
                        definition.SetHeight(length.to_grid_length())?;
                        definitions.Append(&definition)?;
                    }
                }
            }
            written.clear();
            written.extend_from_slice(lengths);
        }
        self.open(node);
        body(self)?;
        self.close()?;
        Ok(node)
    }

    fn set_column_spacing(&mut self, node: NodeId, spacing: f64) -> Result<()> {
        let Control::Grid {
            grid,
            spacing: written,
            ..
        } = &mut self.control_mut(node).control
        else {
            unreachable!("grid keys hold grids")
        };
        if *written != spacing {
            grid.SetColumnSpacing(spacing)?;
            *written = spacing;
        }
        Ok(())
    }
}

/// The height of a [`Ui::grid`] row, or the width of a [`Ui::columns`]
/// column.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Length {
    /// As large as its control wants.
    Auto,
    /// A fixed length in device-independent pixels.
    Pixels(f64),
    /// A share of the space left after the others, by weight.
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
