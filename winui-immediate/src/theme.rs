//! Theme brushes that follow the theme.
//!
//! A brush looked up in code is the one for the theme at lookup time, and
//! only markup `{ThemeResource}` re-resolves it when the theme changes. The
//! stable runtime has no code API for that, so this module keeps the brushes
//! it sets and looks them up again on the element's `ActualThemeChanged`.
//!
//! The lookup goes through the app's resources, which give the app's theme,
//! so a subtree with its own `RequestedTheme` gets the app's brushes. Reading
//! the controls library's `ThemeDictionaries` for a per-theme lookup stopped
//! the whole app from following the system theme, so this module never
//! touches them.

use crate::{
    bindings::{DependencyObject, DependencyProperty, FrameworkElement},
    events::{Delegate, identity},
    widget::{boxed, resource},
};
use ahash::AHashMap;
use std::cell::RefCell;
use windows_collections::IMap;
use windows_core::{IInspectable, Interface, Result, Weak};

/// Sets `property` of `element` to the theme brush `key`, and sets it again
/// whenever the element's theme changes, until [`clear_brush`] or another
/// [`set_brush`] for the same property. `element` must be a
/// `FrameworkElement`.
pub(crate) fn set_brush(
    element: &impl Interface,
    property: &DependencyProperty,
    key: &'static str,
) -> Result<()> {
    element
        .cast::<DependencyObject>()?
        .SetValue(property, &resource::<IInspectable>(key)?)?;
    bind(element, |bound| {
        match bound
            .brushes
            .iter_mut()
            .find(|(known, _)| known == property)
        {
            Some(entry) => entry.1 = key,
            None => bound.brushes.push((property.clone(), key)),
        }
    })
}

/// Clears `property` of `element` back to its default or style value, and
/// stops [`set_brush`] from setting it again.
pub(crate) fn clear_brush(element: &impl Interface, property: &DependencyProperty) -> Result<()> {
    let identity = identity(element)?;
    BOUND.with_borrow_mut(|bound| {
        if let Some(entry) = bound.elements.get_mut(&identity) {
            entry.brushes.retain(|(known, _)| known != property);
        }
    });
    element.cast::<DependencyObject>()?.ClearValue(property)
}

/// Puts into `element`'s resources the lightweight styling `overrides`,
/// pairs of a template's resource key and the theme brush key it takes, and
/// puts them in again whenever the element's theme changes. `element` must
/// be a `FrameworkElement`.
pub(crate) fn style_resources(
    element: &impl Interface,
    overrides: &'static [(&'static str, &'static str)],
) -> Result<()> {
    insert_overrides(&element.cast()?, overrides)?;
    bind(element, |bound| bound.overrides = Some(overrides))
}

/// Takes the `overrides` [`style_resources`] put into `element`'s resources
/// back out, so the template's own brushes apply again.
pub(crate) fn clear_style_resources(
    element: &impl Interface,
    overrides: &'static [(&'static str, &'static str)],
) -> Result<()> {
    let identity = identity(element)?;
    BOUND.with_borrow_mut(|bound| {
        if let Some(entry) = bound.elements.get_mut(&identity) {
            entry.overrides = None;
        }
    });
    let resources = element
        .cast::<FrameworkElement>()?
        .Resources()?
        .cast::<IMap<IInspectable, IInspectable>>()?;
    for (key, _) in overrides {
        let key = boxed(key);
        if resources.HasKey(&key)? {
            resources.Remove(&key)?;
        }
    }
    Ok(())
}

/// Looks up again every brush [`set_brush`] and [`style_resources`] gave
/// `element`, for the theme it now shows. The theme delegate calls it.
pub(crate) fn refresh(element: &FrameworkElement) -> Result<()> {
    let identity = identity(element)?;
    let target = element.cast::<DependencyObject>()?;
    // The borrow holds across `SetValue`, which is safe because only a pass
    // calls the `borrow_mut` functions, and a pass never runs inside a XAML
    // callback.
    BOUND.with_borrow(|bound| {
        let Some(entry) = bound.elements.get(&identity) else {
            return Ok(());
        };
        for (property, key) in &entry.brushes {
            target.SetValue(property, &resource::<IInspectable>(key)?)?;
        }
        if let Some(overrides) = entry.overrides {
            insert_overrides(element, overrides)?;
        }
        Ok(())
    })
}

fn insert_overrides(
    element: &FrameworkElement,
    overrides: &[(&'static str, &'static str)],
) -> Result<()> {
    let resources = element
        .Resources()?
        .cast::<IMap<IInspectable, IInspectable>>()?;
    for (key, brush) in overrides {
        resources.Insert(&boxed(key), &resource::<IInspectable>(brush)?)?;
    }
    Ok(())
}

/// Runs `update` on `element`'s entry, made on first use, which also
/// registers the theme delegate on the element.
fn bind(element: &impl Interface, update: impl FnOnce(&mut Bound)) -> Result<()> {
    let element: FrameworkElement = element.cast()?;
    let identity = identity(&element)?;
    let fresh = BOUND.with_borrow_mut(|bound| -> Result<bool> {
        bound.prune();
        let fresh = !bound.elements.contains_key(&identity);
        if fresh {
            bound.elements.insert(
                identity,
                Bound {
                    element: element.downgrade()?,
                    brushes: Vec::new(),
                    overrides: None,
                },
            );
        }
        update(bound.elements.get_mut(&identity).expect("inserted above"));
        Ok(fresh)
    })?;
    if fresh {
        DELEGATE.with(|delegate| {
            delegate.register(
                &element.cast::<crate::bindings::IFrameworkElement>()?,
                |table| table.ActualThemeChanged,
            )
        })?;
    }
    Ok(())
}

/// The brushes this module keeps, by element identity.
#[derive(Default)]
struct Brushes {
    elements: AHashMap<usize, Bound>,
    /// The entry count after the last prune.
    pruned: usize,
}

impl Brushes {
    /// Drops the entries of released elements, once the map has doubled
    /// since the last prune, so the cost stays linear.
    fn prune(&mut self) {
        if self.elements.len() < 2 * self.pruned.max(64) {
            return;
        }
        self.elements
            .retain(|_, entry| entry.element.upgrade().is_some());
        self.pruned = self.elements.len();
    }
}

struct Bound {
    element: Weak<FrameworkElement>,
    brushes: Vec<(DependencyProperty, &'static str)>,
    overrides: Option<&'static [(&'static str, &'static str)]>,
}

thread_local! {
    static BOUND: RefCell<Brushes> = RefCell::default();
    static DELEGATE: Delegate = Delegate::theme_changed();
}
