use super::Control;
use crate::{
    bindings::{
        Grid, Panel, UIElement, UIElementCollection,
        composition::{
            CompositionAnimation, CompositionBatchTypes, CompositionBrush,
            CompositionEasingFunction, CompositionEffectBrush, CompositionEffectFactory,
            CompositionObject, CompositionVisualSurface, Compositor, ContainerVisual,
            ElementCompositionPreview, ICompositionSurface, ICompositorWithVisualSurface,
            KeyFrameAnimation, SpriteVisual, UIElement as Element, Visual,
        },
    },
    pass::{ChildKind, ControlNode, Ui},
    widget::Response,
};
use std::{cell::RefCell, hash::Hash, time::Duration};
use windows_core::{EventRevoker, HSTRING, Interface, Result};
use windows_numerics::{Vector2, Vector3};

mod blur;

/// How long [`Ui::morph`] takes to change controls.
pub const MORPH_DURATION: Duration = Duration::from_millis(250);
/// The blur, as a standard deviation in view pixels, of a control that has
/// fully morphed out.
const BLUR_OUT: f32 = 6.0;
/// Room around a control's pixels for its blur to spread into: three
/// standard deviations, where a Gaussian has all but faded.
const PAD: f32 = 3.0 * BLUR_OUT;

impl Ui {
    /// Shows one of the controls `body` declares, the one at index `shown`
    /// in declaration order, with the others stacked in the same cell. When
    /// `shown` changes, the old control blurs and fades out while the new
    /// one sharpens and fades in, over [`MORPH_DURATION`]. A `shown` past
    /// the last control shows none.
    ///
    /// The hidden controls stay in the tree at zero opacity, so the morph
    /// is as large as the largest, and screen readers still find them;
    /// name the control around the morph instead, such as its button.
    /// While a morph runs, a copy of each control's pixels draws over it,
    /// so do not morph a secret, which the copy would keep on screen until
    /// the morph ends. A control declared while hidden, or before its first
    /// layout, appears and disappears without the morph.
    pub fn morph(
        &mut self,
        id: impl Hash,
        shown: usize,
        body: impl FnOnce(&mut Ui) -> Result<()>,
    ) -> Result<Response<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let grid = Grid::new()?;
            let children = grid.cast::<Panel>()?.Children()?;
            Ok(ControlNode::new(
                grid.cast()?,
                Control::Morph(Box::new(Parts {
                    grid,
                    children,
                    shown: None,
                    count: 0,
                    container: None,
                    sprites: Vec::new(),
                    done: None,
                })),
            ))
        })?;
        self.open(node);
        body(self)?;
        self.close()?;
        let control = self.control(node);
        let Control::Morph(parts) = &control.control else {
            unreachable!("morph keys hold morphs")
        };
        if parts.shown != Some(shown) || parts.count != control.visual.len() {
            let children: Vec<UIElement> = control
                .visual
                .iter()
                .map(|&child| self.control(child).element.clone())
                .collect();
            let Control::Morph(parts) = &mut self.control_mut(node).control else {
                unreachable!("morph keys hold morphs")
            };
            parts.show(&children, shown)?;
        }
        Ok(Response { ui: self, node })
    }
}

/// While no morph runs, the child at `shown` has opacity 1 and every other
/// child and every sprite has opacity 0. While one runs, every child has
/// opacity 0 and the sprites draw; `done` ends it.
pub(crate) struct Parts {
    grid: Grid,
    /// The grid's children, for [`Control::children`].
    pub(crate) children: UIElementCollection,
    /// [`None`] before the first pass.
    shown: Option<usize>,
    /// How many children the last pass declared.
    count: usize,
    /// Holds the sprites over the grid's children. Made at the first morph.
    container: Option<ContainerVisual>,
    /// By child index; [`None`] for a child that has not morphed yet.
    sprites: Vec<Option<Sprite>>,
    /// Revokes the last morph's completion handler when dropped, so a
    /// superseded morph never shows its child.
    done: Option<EventRevoker>,
}

/// Draws a blurred copy of `element`'s pixels.
struct Sprite {
    element: UIElement,
    visual: SpriteVisual,
    surface: CompositionVisualSurface,
    brush: CompositionEffectBrush,
}

impl Parts {
    fn show(&mut self, children: &[UIElement], shown: usize) -> Result<()> {
        let old = self
            .shown
            .filter(|&old| old != shown && self.count == children.len() && old < children.len());
        self.shown = Some(shown);
        self.count = children.len();
        if let Some(old) = old
            && shown < children.len()
            && self.morph(children, old, shown)?
        {
            return Ok(());
        }
        self.jump(children, shown)
    }

    /// Shows `shown` at once.
    fn jump(&mut self, children: &[UIElement], shown: usize) -> Result<()> {
        self.done = None;
        for sprite in self.sprites.iter().flatten() {
            sprite.visual.cast::<Visual>()?.SetOpacity(0.0)?;
        }
        for (index, child) in children.iter().enumerate() {
            reveal(child, index == shown)?;
        }
        Ok(())
    }

    /// Starts the morph from `old` to `new`. Returns false, having changed
    /// nothing, when either child has no size yet.
    fn morph(&mut self, children: &[UIElement], old: usize, new: usize) -> Result<bool> {
        let mut places = [(Vector2::default(), Vector3::default()); 2];
        for (place, index) in places.iter_mut().zip([old, new]) {
            let element = children[index].cast::<Element>()?;
            *place = (element.ActualSize()?, element.ActualOffset()?);
            if place.0.x == 0.0 || place.0.y == 0.0 {
                return Ok(false);
            }
        }
        // Revoke first: restarting an animation completes the batch that
        // ran it.
        self.done = None;
        let compositor =
            ElementCompositionPreview::GetElementVisual(&self.grid.cast::<Element>()?)?
                .cast::<CompositionObject>()?
                .Compositor()?;
        let container = match &self.container {
            Some(container) => container.clone(),
            None => {
                let container = compositor.CreateContainerVisual()?;
                ElementCompositionPreview::SetElementChildVisual(
                    &self.grid.cast::<Element>()?,
                    &container.cast::<Visual>()?,
                )?;
                self.container = Some(container.clone());
                container
            }
        };
        self.sprites.resize_with(children.len(), || None);
        for index in [old, new] {
            let stale = self.sprites[index]
                .as_ref()
                .is_some_and(|sprite| sprite.element != children[index]);
            if stale && let Some(sprite) = self.sprites[index].take() {
                container
                    .Children()?
                    .Remove(&sprite.visual.cast::<Visual>()?)?;
            }
            if self.sprites[index].is_none() {
                self.sprites[index] = Some(sprite(&compositor, &container, &children[index])?);
            }
        }
        for (index, sprite) in self.sprites.iter().enumerate() {
            if let Some(sprite) = sprite
                && index != old
                && index != new
            {
                sprite.visual.cast::<Visual>()?.SetOpacity(0.0)?;
            }
        }
        let [outgoing, incoming] =
            [old, new].map(|index| self.sprites[index].as_ref().expect("made above"));
        for (sprite, (size, offset)) in [outgoing, incoming].into_iter().zip(places) {
            let padded = Vector2 {
                x: size.x + 2.0 * PAD,
                y: size.y + 2.0 * PAD,
            };
            sprite
                .surface
                .SetSourceOffset(Vector2 { x: -PAD, y: -PAD })?;
            sprite.surface.SetSourceSize(padded)?;
            let visual = sprite.visual.cast::<Visual>()?;
            visual.SetSize(padded)?;
            visual.SetOffset(Vector3 {
                x: offset.x - PAD,
                y: offset.y - PAD,
                z: 0.0,
            })?;
        }
        for child in children {
            reveal(child, false)?;
        }
        let batch = compositor.CreateScopedBatch(CompositionBatchTypes::Animation)?;
        animate(&compositor, outgoing, (1.0, 0.0), (0.0, BLUR_OUT))?;
        animate(&compositor, incoming, (0.0, 1.0), (BLUR_OUT, 0.0))?;
        batch.End()?;
        let child = children[new].clone();
        let visuals = [outgoing, incoming].map(|sprite| sprite.visual.clone());
        self.done = Some(batch.Completed(move |_, _| {
            let finish = || -> Result<()> {
                reveal(&child, true)?;
                for visual in &visuals {
                    visual.cast::<Visual>()?.SetOpacity(0.0)?;
                }
                Ok(())
            };
            // The handler has no caller to report to. A failed write leaves
            // the sprites drawing, and the next morph or jump writes every
            // opacity again.
            let _ = finish();
        })?);
        Ok(true)
    }
}

/// Shows or hides a child. A hidden child takes no pointer input, so the
/// shown one under it does.
fn reveal(child: &UIElement, shown: bool) -> Result<()> {
    child
        .cast::<Element>()?
        .SetOpacity(if shown { 1.0 } else { 0.0 })?;
    child.SetIsHitTestVisible(shown)
}

/// A hidden sprite over `element`, at the top of `container`.
fn sprite(
    compositor: &Compositor,
    container: &ContainerVisual,
    element: &UIElement,
) -> Result<Sprite> {
    let surface = compositor
        .cast::<ICompositorWithVisualSurface>()?
        .CreateVisualSurface()?;
    // The surface draws the element's visual even at zero opacity, which
    // the morph spike in winui-spikes measured.
    surface.SetSourceVisual(&ElementCompositionPreview::GetElementVisual(
        &element.cast::<Element>()?,
    )?)?;
    let source =
        compositor.CreateSurfaceBrushWithSurface(&surface.cast::<ICompositionSurface>()?)?;
    let brush = blur_factory(compositor)?.CreateBrush()?;
    brush.SetSourceParameter(blur::SOURCE, &source.cast::<CompositionBrush>()?)?;
    let visual = compositor.CreateSpriteVisual()?;
    visual.SetBrush(&brush.cast::<CompositionBrush>()?)?;
    visual.cast::<Visual>()?.SetOpacity(0.0)?;
    container
        .Children()?
        .InsertAtTop(&visual.cast::<Visual>()?)?;
    Ok(Sprite {
        element: element.clone(),
        visual,
        surface,
        brush,
    })
}

thread_local! {
    /// The blur factory of the thread's compositor. Making one compiles the
    /// effect, so every morph shares it.
    static FACTORY: RefCell<Option<CompositionEffectFactory>> = const { RefCell::new(None) };
}

fn blur_factory(compositor: &Compositor) -> Result<CompositionEffectFactory> {
    FACTORY.with_borrow_mut(|factory| {
        if let Some(factory) = factory {
            return Ok(factory.clone());
        }
        let made = compositor.CreateEffectFactoryWithProperties(
            &blur::effect()?,
            &windows_collections::IIterable::<HSTRING>::from(vec![HSTRING::from(blur::AMOUNT)]),
        )?;
        *factory = Some(made.clone());
        Ok(made)
    })
}

/// Animates `sprite`'s opacity and blur from the first value of each pair
/// to the second.
fn animate(
    compositor: &Compositor,
    sprite: &Sprite,
    opacity: (f32, f32),
    blur: (f32, f32),
) -> Result<()> {
    // Ease in and out, so the two controls cross at the middle. The default
    // curve is front-loaded: the old control was gone and the new one
    // nearly sharp in the first ~100 ms.
    let easing = compositor
        .CreateCubicBezierEasingFunction(Vector2 { x: 0.42, y: 0.0 }, Vector2 { x: 0.58, y: 1.0 })?
        .cast::<CompositionEasingFunction>()?;
    let keyframes = |(from, to): (f32, f32)| -> Result<CompositionAnimation> {
        let animation = compositor.CreateScalarKeyFrameAnimation()?;
        animation.InsertKeyFrame(0.0, from)?;
        animation.InsertKeyFrameWithEasingFunction(1.0, to, &easing)?;
        animation.cast::<KeyFrameAnimation>()?.SetDuration(
            windows_time::TimeSpan::try_from(MORPH_DURATION).expect("250 ms fits a TimeSpan"),
        )?;
        animation.cast()
    };
    sprite
        .visual
        .cast::<CompositionObject>()?
        .StartAnimation("Opacity", &keyframes(opacity)?)?;
    sprite
        .brush
        .cast::<CompositionObject>()?
        .StartAnimation(blur::AMOUNT, &keyframes(blur)?)
}
