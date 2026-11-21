use super::{Control, Response, boxed, resource};
use crate::{
    bindings::{
        AutomationProperties, Brush, ColumnDefinition, ContentControl, DependencyObject, FontIcon,
        FrameworkElement, Grid, GridLength, GridUnitType, INavigationView2, IconElement,
        NavigationView, NavigationViewBackButtonVisible, NavigationViewItem,
        NavigationViewItemHeader, NavigationViewItemSeparator, NavigationViewPaneDisplayMode,
        Panel, TextBlock, TextTrimming, ToolTipService, UIElement, VerticalAlignment,
    },
    events::{EventKind, identity},
    pass::{ChildKind, ControlNode, Ui},
    tree::NodeId,
};
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};
use windows_core::{IInspectable, Interface, Result};

impl Ui {
    /// A WinUI `NavigationView`: a pane of entries on the left, with
    /// Settings at its bottom, and the one control `body` declares to its
    /// right. The pane is 240px wide from 1000px of window width, a strip
    /// of icons from 640px, and a menu button below that; a collapsed item
    /// shows its label as a tooltip.
    ///
    /// `entries` pushes the pane's entries, and runs only on the first pass
    /// and when `revision` differs from the last pass's, so a steady pass
    /// builds nothing. Change `revision` whenever any entry would change,
    /// counts included, or the pane keeps showing the old ones.
    ///
    /// `selected` is what the pane shows as selected. When the user picks
    /// another entry, the next pass writes it into `selected` and
    /// [`picked`](NavigationViewResponse::picked) returns true. A `selected`
    /// that names a header or a separator, or an index past the end, shows
    /// nothing selected but stays as it is. `body` gets `selected` after
    /// the pick.
    ///
    /// The view has no back button, because the window's title bar holds
    /// the one back button.
    pub fn navigation_view<'a>(
        &mut self,
        id: impl Hash,
        revision: u64,
        entries: impl FnOnce(&mut Vec<NavigationEntry<'a>>),
        selected: &mut Option<NavigationSelection>,
        body: impl FnOnce(&mut Ui, Option<NavigationSelection>) -> Result<()>,
    ) -> Result<NavigationViewResponse<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let view = NavigationView::new()?;
            // The generator puts these on the interface that added them.
            let added = view.cast::<INavigationView2>()?;
            added.SetPaneDisplayMode(NavigationViewPaneDisplayMode::Auto)?;
            added.SetIsBackButtonVisible(NavigationViewBackButtonVisible::Collapsed)?;
            view.SetOpenPaneLength(PANE_WIDTH)?;
            view.SetCompactModeThresholdWidth(COMPACT_FROM)?;
            view.SetExpandedModeThresholdWidth(EXPANDED_FROM)?;
            view.SetIsSettingsVisible(true)?;
            Ok(ControlNode::new(
                view.cast()?,
                Control::NavigationView(Box::new(Parts {
                    view,
                    written: Vec::new(),
                    revision: None,
                    selected: None,
                    picked: false,
                    content: None,
                })),
            ))
        })?;

        // Subscribed before any write, so a write's own event comes back
        // and reads as no pick.
        let control = self.control_mut(node);
        let element = control.element.clone();
        let mut subscribed = control.subscribed;
        self.events.subscribe(
            node,
            &mut subscribed,
            &element,
            EventKind::NavigationSelectionChanged,
        )?;
        self.control_mut(node).subscribed = subscribed;
        let changed = self
            .events
            .happened(node, EventKind::NavigationSelectionChanged);

        let parts = self.parts_mut(node);
        parts.picked = false;
        // The event also follows this layer's own writes; those leave the
        // view showing what was written, so only a difference is a pick.
        // A pick of nothing (the menu was cleared) is not the user's.
        if changed
            && let Some(shown) = parts.shown()?
            && parts.selected != Some(shown)
        {
            parts.selected = Some(shown);
            parts.picked = *selected != Some(shown);
            *selected = Some(shown);
        }
        if parts.revision != Some(revision) {
            let mut declared = Vec::new();
            entries(&mut declared);
            if parts.write_entries(&declared)? {
                // New items select nothing until written again.
                parts.selected = None;
            }
            parts.revision = Some(revision);
        }
        let shown = selected.filter(|selection| parts.selectable(*selection));
        if parts.selected != shown {
            parts.select(shown)?;
            parts.selected = shown;
        }
        let selected = *selected;
        self.open(node);
        body(self, selected)?;
        self.close()?;
        Ok(NavigationViewResponse(Response { ui: self, node }))
    }

    fn parts_mut(&mut self, node: NodeId) -> &mut Parts {
        match &mut self.control_mut(node).control {
            Control::NavigationView(parts) => parts,
            _ => unreachable!("navigation view keys hold navigation views"),
        }
    }
}

/// One line of a [`Ui::navigation_view`] pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationEntry<'a> {
    /// A small heading over the items after it. Collapsed panes hide it.
    Header(&'a str),
    /// A selectable item: a Segoe Fluent Icons `glyph`, its `label`, and a
    /// `count` at the right edge in the secondary color.
    Item {
        glyph: char,
        label: &'a str,
        count: Option<usize>,
    },
    Separator,
}

/// What a [`Ui::navigation_view`] pane has selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationSelection {
    /// The item at this index of the entries.
    Entry(usize),
    Settings,
}

/// What [`Ui::navigation_view`] returns.
pub struct NavigationViewResponse<'u>(Response<'u>);

impl NavigationViewResponse<'_> {
    /// Whether this pass wrote the user's pick into `selected`.
    pub fn picked(&self) -> bool {
        match &self.0.ui.control(self.0.node).control {
            Control::NavigationView(parts) => parts.picked,
            _ => unreachable!("navigation view keys hold navigation views"),
        }
    }
}

impl<'u> Deref for NavigationViewResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.0
    }
}

impl DerefMut for NavigationViewResponse<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// The pane widths from the VaultScreen design, in view pixels.
const PANE_WIDTH: f64 = 240.0;
const COMPACT_FROM: f64 = 640.0;
const EXPANDED_FROM: f64 = 1000.0;

/// A navigation view and what this layer last wrote to it.
///
/// Contract: `written` holds one entry per object in the view's
/// `MenuItems`, in the same order.
pub(crate) struct Parts {
    view: NavigationView,
    written: Vec<Written>,
    /// The revision `written` was built for; [`None`] before the first.
    revision: Option<u64>,
    /// The selection as last written or read back.
    selected: Option<NavigationSelection>,
    /// This pass's pick, for [`NavigationViewResponse::picked`].
    picked: bool,
    /// The content control as last applied.
    pub(crate) content: Option<NodeId>,
}

/// One menu object and the values written to it.
struct Written {
    object: IInspectable,
    kind: Kind,
}

enum Kind {
    Header {
        label: String,
    },
    Item {
        item: NavigationViewItem,
        icon: FontIcon,
        label_block: TextBlock,
        count_block: TextBlock,
        glyph: char,
        label: String,
        count: Option<usize>,
    },
    Separator,
}

impl Kind {
    /// Whether `entry` can be written into this object, or needs a new
    /// one.
    fn fits(&self, entry: &NavigationEntry<'_>) -> bool {
        matches!(
            (self, entry),
            (Kind::Header { .. }, NavigationEntry::Header(_))
                | (Kind::Item { .. }, NavigationEntry::Item { .. })
                | (Kind::Separator, NavigationEntry::Separator)
        )
    }
}

impl Parts {
    /// Makes the menu show `entries`. Returns whether it rebuilt the menu,
    /// which drops WinUI's selection.
    fn write_entries(&mut self, entries: &[NavigationEntry<'_>]) -> Result<bool> {
        let same_shape = self.written.len() == entries.len()
            && self
                .written
                .iter()
                .zip(entries)
                .all(|(written, entry)| written.kind.fits(entry));
        if !same_shape {
            let menu = self.view.MenuItems()?;
            menu.Clear()?;
            self.written.clear();
            for entry in entries {
                let written = Written::new(entry)?;
                menu.Append(&written.object)?;
                self.written.push(written);
            }
            return Ok(true);
        }
        for (written, entry) in self.written.iter_mut().zip(entries) {
            written.update(entry)?;
        }
        Ok(false)
    }

    /// Whether `selection` names something the pane can select.
    fn selectable(&self, selection: NavigationSelection) -> bool {
        match selection {
            NavigationSelection::Settings => true,
            NavigationSelection::Entry(index) => self
                .written
                .get(index)
                .is_some_and(|written| matches!(written.kind, Kind::Item { .. })),
        }
    }

    fn select(&self, selection: Option<NavigationSelection>) -> Result<()> {
        match selection {
            None => self.view.SetSelectedItem(None::<&IInspectable>),
            // Before the template loads there is no settings item yet;
            // selecting nothing then is the closest the view can show.
            Some(NavigationSelection::Settings) => match self.view.SettingsItem() {
                Ok(settings) => self.view.SetSelectedItem(&settings),
                Err(_) => self.view.SetSelectedItem(None::<&IInspectable>),
            },
            Some(NavigationSelection::Entry(index)) => {
                self.view.SetSelectedItem(&self.written[index].object)
            }
        }
    }

    /// What WinUI shows as selected now.
    fn shown(&self) -> Result<Option<NavigationSelection>> {
        // An empty selection reads as a null object, which fails to convert.
        let Ok(item) = self.view.SelectedItem() else {
            return Ok(None);
        };
        let item = identity(&item)?;
        // The settings item is null until the view's template loads, and
        // then it cannot be the selected item.
        if let Ok(settings) = self.view.SettingsItem()
            && identity(&settings)? == item
        {
            return Ok(Some(NavigationSelection::Settings));
        }
        for (index, written) in self.written.iter().enumerate() {
            if identity(&written.object)? == item {
                return Ok(Some(NavigationSelection::Entry(index)));
            }
        }
        Ok(None)
    }
}

impl Written {
    fn new(entry: &NavigationEntry<'_>) -> Result<Self> {
        let mut written = match entry {
            NavigationEntry::Header(_) => {
                let header = NavigationViewItemHeader::new()?;
                Written {
                    object: header.cast()?,
                    kind: Kind::Header {
                        label: String::new(),
                    },
                }
            }
            NavigationEntry::Separator => Written {
                object: NavigationViewItemSeparator::new()?.cast()?,
                kind: Kind::Separator,
            },
            NavigationEntry::Item { .. } => {
                let item = NavigationViewItem::new()?;
                let icon = FontIcon::new()?;
                item.SetIcon(&icon.cast::<IconElement>()?)?;
                // The label takes the free width and the count sits at the
                // right edge, as in the design.
                let grid = Grid::new()?;
                let columns = grid.ColumnDefinitions()?;
                for (value, grid_unit_type) in
                    [(1.0, GridUnitType::Star), (1.0, GridUnitType::Auto)]
                {
                    let column = ColumnDefinition::new()?;
                    column.SetWidth(GridLength {
                        value,
                        grid_unit_type,
                    })?;
                    columns.Append(&column)?;
                }
                let label_block = TextBlock::new()?;
                label_block.SetTextTrimming(TextTrimming::CharacterEllipsis)?;
                let count_block = TextBlock::new()?;
                count_block.SetFontSize(12.0)?;
                count_block.SetForeground(&resource::<Brush>("TextFillColorSecondaryBrush")?)?;
                let count_frame = count_block.cast::<FrameworkElement>()?;
                count_frame.SetVerticalAlignment(VerticalAlignment::Center)?;
                count_frame.SetMargin(crate::bindings::Thickness {
                    left: 8.0,
                    top: 0.0,
                    right: 0.0,
                    bottom: 0.0,
                })?;
                Grid::SetColumn(&count_frame, 1)?;
                let children = grid.cast::<Panel>()?.Children()?;
                children.Append(&label_block.cast::<UIElement>()?)?;
                children.Append(&count_block.cast::<UIElement>()?)?;
                item.cast::<ContentControl>()?.SetContent(&grid)?;
                Written {
                    object: item.cast()?,
                    kind: Kind::Item {
                        item,
                        icon,
                        label_block,
                        count_block,
                        // Written by `update` below, which sees a change
                        // from these.
                        glyph: '\0',
                        label: String::new(),
                        count: Some(usize::MAX),
                    },
                }
            }
        };
        written.update(entry)?;
        Ok(written)
    }

    /// Writes the values of `entry` that differ from the last ones. The
    /// kind must fit, as [`Kind::fits`] checks.
    fn update(&mut self, entry: &NavigationEntry<'_>) -> Result<()> {
        match (&mut self.kind, entry) {
            (Kind::Header { label }, NavigationEntry::Header(text)) => {
                if label != text {
                    self.object
                        .cast::<ContentControl>()?
                        .SetContent(&boxed(text))?;
                    label.clear();
                    label.push_str(text);
                }
            }
            (
                Kind::Item {
                    item,
                    icon,
                    label_block,
                    count_block,
                    glyph,
                    label,
                    count,
                },
                NavigationEntry::Item {
                    glyph: new_glyph,
                    label: new_label,
                    count: new_count,
                },
            ) => {
                if glyph != new_glyph {
                    icon.SetGlyph(&*new_glyph.encode_utf8(&mut [0; 4]))?;
                    *glyph = *new_glyph;
                }
                if label != new_label {
                    label_block.SetText(new_label)?;
                    let target = item.cast::<DependencyObject>()?;
                    // The count is left out of the name, so a screen reader
                    // reads the item as its label.
                    AutomationProperties::SetName(&target, new_label)?;
                    ToolTipService::SetToolTip(&target, &boxed(new_label))?;
                    label.clear();
                    label.push_str(new_label);
                }
                if count != new_count {
                    match new_count {
                        Some(new_count) => count_block.SetText(&new_count.to_string())?,
                        None => count_block.SetText("")?,
                    }
                    *count = *new_count;
                }
            }
            (Kind::Separator, NavigationEntry::Separator) => {}
            _ => unreachable!("update is called only on a kind that fits"),
        }
        Ok(())
    }
}
