use super::{Control, Response};
use crate::{
    bindings::{
        DispatcherQueue, DispatcherQueueTimer, FrameworkElement, Grid, Panel, Rect,
        RectangleGeometry, UIElement, UIElementCollection, Vector3Transition,
    },
    pass::{ChildKind, ControlNode, Ui},
    tree::NodeId,
};
use std::{hash::Hash, time::Duration};
use windows_core::{Interface, Result};

impl Ui {
    /// Shows the one control `body` declares as the screen named `screen`.
    ///
    /// The controls `body` declares belong to `screen`: when `screen` differs
    /// from the last pass's, they are all new, and the old screen's controls
    /// are dropped with their state, as if they were never declared.
    ///
    /// A new screen slides in from the side `slide` names while the old one
    /// slides out the other side, over [`SLIDE_DURATION`]. During the slide
    /// both screens are drawn, so the navigator is as large as the larger
    /// of the two; the old one takes no input. [`Slide::Instant`] removes
    /// the old screen from the visual tree before this call returns, which
    /// is what a screen that hid secrets needs.
    ///
    /// The first screen, and a screen change before the navigator has been
    /// laid out, appear at once whatever `slide` says.
    pub fn navigator(
        &mut self,
        id: impl Hash,
        screen: impl Hash,
        slide: Slide,
        body: impl FnOnce(&mut Ui) -> Result<()>,
    ) -> Result<Response<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let grid = Grid::new()?;
            let children = grid.cast::<Panel>()?.Children()?;
            Ok(ControlNode::new(
                grid.cast()?,
                Control::Navigator(Box::new(Navigator {
                    grid,
                    children,
                    content: None,
                    slide,
                    leaving: Vec::new(),
                })),
            ))
        })?;
        let Control::Navigator(navigator) = &mut self.control_mut(node).control else {
            unreachable!("navigator keys hold navigators")
        };
        navigator.slide = slide;
        // A finished timer already took its element out of the grid.
        navigator
            .leaving
            .retain(|leaving| leaving.timer.IsRunning().unwrap_or(false));
        self.open_scoped(node, screen);
        body(self)?;
        self.close()?;
        Ok(Response { ui: self, node })
    }
}

/// How long a screen takes to slide in or out.
pub const SLIDE_DURATION: Duration = Duration::from_millis(250);

/// How long the old screen stays in the grid after a slide starts: the slide
/// and a margin, because the timer and the animation run on separate clocks.
const REMOVE_AFTER: Duration = Duration::from_millis(350);

/// How a [`Ui::navigator`] changes from one screen to the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slide {
    /// The new screen comes in from the right, the old one leaves left.
    Forward,
    /// The new screen comes in from the left, the old one leaves right.
    Back,
    /// The new screen replaces the old one at once.
    Instant,
}

pub(crate) struct Navigator {
    grid: Grid,
    children: UIElementCollection,
    /// The screen's control as last applied.
    pub(crate) content: Option<NodeId>,
    /// What the current pass asks for, used when the screen changes.
    slide: Slide,
    /// Old screens still sliding out, each removed from `children` by its
    /// timer.
    leaving: Vec<Leaving>,
}

struct Leaving {
    element: UIElement,
    timer: DispatcherQueueTimer,
    /// Dropping it ends the tick subscription.
    _tick: windows_core::EventRevoker,
}

impl Navigator {
    /// Shows `entering` in place of `old`, as `slide` asks.
    pub(crate) fn swap(
        &mut self,
        old: Option<UIElement>,
        entering: Option<UIElement>,
    ) -> Result<()> {
        let width = self.grid.cast::<FrameworkElement>()?.ActualWidth()?;
        let sign = match self.slide {
            Slide::Forward => 1.0,
            Slide::Back => -1.0,
            Slide::Instant => 0.0,
        };
        let (Some(old), Some(entering)) = (&old, &entering) else {
            return self.cut(old, entering);
        };
        if sign == 0.0 || width <= 0.0 {
            return self.cut(Some(old.clone()), Some(entering.clone()));
        }
        let width = width as f32;
        // Clip the sides only: during the slide the grid may be shorter than
        // the taller screen, which must not lose its bottom.
        let clip = RectangleGeometry::new()?;
        clip.SetRect(Rect {
            x: 0.0,
            y: -CLIP_REACH,
            width,
            height: 2.0 * CLIP_REACH,
        })?;
        self.grid.cast::<UIElement>()?.SetClip(&clip)?;

        // Translation set before the transition jumps; set after, it slides.
        entering.SetTranslation(offset(sign * width))?;
        self.children.Append(entering)?;
        entering.SetTranslationTransition(&transition()?)?;
        entering.SetTranslation(offset(0.0))?;
        old.SetIsHitTestVisible(false)?;
        old.SetTranslationTransition(&transition()?)?;
        old.SetTranslation(offset(-sign * width))?;

        let timer = DispatcherQueue::GetForCurrentThread()?.CreateTimer()?;
        timer.SetInterval(span(REMOVE_AFTER))?;
        timer.SetIsRepeating(false)?;
        let children = self.children.clone();
        let element = old.clone();
        let tick = timer.Tick(move |sender, _| {
            // A tick has no caller to return an error to. A failed Stop
            // leaves a timer whose next tick finds nothing to remove.
            if let Some(sender) = sender.as_ref() {
                let _ = sender.Stop();
            }
            if let Err(error) = remove(&children, &element) {
                tracing::warn!(%error, "navigator could not remove the old screen");
            }
        })?;
        timer.Start()?;
        self.leaving.push(Leaving {
            element: old.clone(),
            timer,
            _tick: tick,
        });
        Ok(())
    }

    /// Replaces `old` with `entering` with no animation, and drops every
    /// screen still sliding out.
    fn cut(&mut self, old: Option<UIElement>, entering: Option<UIElement>) -> Result<()> {
        self.stop()?;
        for leaving in self.leaving.drain(..) {
            remove(&self.children, &leaving.element)?;
        }
        if let Some(old) = old {
            remove(&self.children, &old)?;
        }
        if let Some(entering) = entering {
            entering.SetTranslation(offset(0.0))?;
            self.children.Append(&entering)?;
        }
        Ok(())
    }

    /// Stops the timers of the screens sliding out, so none fires after the
    /// navigator is gone.
    pub(crate) fn stop(&self) -> Result<()> {
        for leaving in &self.leaving {
            leaving.timer.Stop()?;
        }
        Ok(())
    }
}

/// Half the height of the clip, far beyond any screen.
const CLIP_REACH: f32 = 100_000.0;

fn remove(children: &UIElementCollection, element: &UIElement) -> Result<()> {
    let mut index = 0;
    if children.IndexOf(element, &mut index)? {
        children.RemoveAt(index)?;
    }
    Ok(())
}

fn offset(x: f32) -> windows_numerics::Vector3 {
    windows_numerics::Vector3 { x, y: 0.0, z: 0.0 }
}

fn span(duration: Duration) -> windows_time::TimeSpan {
    windows_time::TimeSpan::try_from(duration).expect("a fraction of a second fits a TimeSpan")
}

fn transition() -> Result<Vector3Transition> {
    let transition = Vector3Transition::new()?;
    transition.SetDuration(span(SLIDE_DURATION))?;
    Ok(transition)
}
