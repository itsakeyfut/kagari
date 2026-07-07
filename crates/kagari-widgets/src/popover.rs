//! The [`Popover`] widget (#81): an anchored floating container whose visibility is a controlled
//! `RwSignal<bool>`.
//!
//! While `open` is true, `content` renders in an anchored [`overlay()`](kagari_core::overlay()) over the
//! `anchor` element; an outside press / Esc dismisses it (sets `open` false) via the #219 substrate. This
//! is the **interactive** overlay path (`.open()`: paint-gate + focus save/restore + dismiss layer) — the
//! counterpart of the [`Tooltip`](crate::Tooltip)'s non-interactive `dyn_if`-only path (RK-039).
//!
//! The content mounts via `dyn_if(open)` **inside** the overlay, so its focusables register only while
//! open. The `dyn_if` is inside (not around) the overlay so the overlay stays mounted and its open-state
//! effect can restore focus to the trigger on close.

use kagari_core::element::dyn_if;
use kagari_core::reactive::RwSignal;
use kagari_core::reactive::prelude::*;
use kagari_core::{AnchorHandle, AnyElement, IntoElement, Placement, overlay};

/// A popover's content **builder**: called to build the floating content each time the popover opens (a
/// closure, so its focusables mount/unmount with the open state — a pre-built element could not remount).
type PopoverContent = Box<dyn Fn() -> AnyElement>;

/// An anchored floating container bound to a `bool` signal (#81). Build with [`popover`]; chain
/// `.placement(..)` / `.dismiss_on_outside(..)`. Returns `impl IntoElement`. While `open` is true the
/// content renders anchored over the `anchor` element; an outside press / Esc sets `open` false (#219).
pub struct Popover {
    anchor: AnchorHandle,
    open: RwSignal<bool>,
    content: PopoverContent,
    placement: Placement,
    dismiss_on_outside: bool,
}

/// Creates a popover anchored to `anchor` (tag the trigger with `div().anchor_ref(&anchor)`), its
/// visibility controlled by `open`, showing `content` (a builder closure, rebuilt each time it opens)
/// while open. Placed anywhere in the tree; the overlay draws on the #219 portal, not in normal flow.
pub fn popover<C: IntoElement>(
    anchor: &AnchorHandle,
    open: RwSignal<bool>,
    content: impl Fn() -> C + 'static,
) -> Popover {
    Popover {
        anchor: anchor.clone(),
        open,
        content: Box::new(move || content().into_element()),
        placement: Placement::Below,
        dismiss_on_outside: true,
    }
}

impl Popover {
    /// Sets the anchor placement relative to the trigger (default [`Placement::Below`]; the core clamps
    /// the popover on-screen and `Auto` flips it to fit).
    pub fn placement(mut self, placement: Placement) -> Self {
        self.placement = placement;
        self
    }

    /// Whether an outside press / Esc dismisses the popover (default `true`; #219). Set `false` for a
    /// popover dismissed only by toggling `open` yourself.
    pub fn dismiss_on_outside(mut self, dismiss: bool) -> Self {
        self.dismiss_on_outside = dismiss;
        self
    }
}

impl IntoElement for Popover {
    fn into_element(self) -> AnyElement {
        let Popover {
            anchor,
            open,
            content,
            placement,
            dismiss_on_outside,
        } = self;

        // `dyn_if(open, content)` is INSIDE the overlay: the overlay stays mounted (paint-gated by
        // `.open()`), so its open-state effect observes the full open→close transition and restores focus
        // to the trigger; only the content (and its focusables) mounts/unmounts with `open`.
        overlay(dyn_if(move || open.get(), content))
            .anchor(&anchor, placement)
            .open(open)
            .dismiss_on_outside(dismiss_on_outside)
            .into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use kagari_base::{Color, Size as BSize};
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::event::FocusRegistry;
    use kagari_core::overlay::{OverlayLayers, OverlayRegistry};
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::{Owner, provide_context};
    use kagari_core::{HitTest, div};
    use kagari_layout::LayoutTree;
    use kagari_render::{Background, Scene};
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};

    const VIEWPORT: BSize = BSize { w: 200.0, h: 200.0 };
    /// The overlay portal's high painter-order band (mirrors `overlay.rs`): an open popover's content
    /// paints here.
    const OVERLAY_BAND: u32 = 1 << 28;

    fn content_bg() -> Background {
        Background::Solid(Color::new(0.0, 0.0, 1.0, 1.0))
    }

    /// A 60x30 blue content box (a content-sized bubble via a fixed inner).
    fn content() -> impl IntoElement {
        div()
            .size(BSize { w: 60.0, h: 30.0 })
            .background(content_bg())
    }

    struct Harness {
        root: AnyElement,
        arena: Arena,
        layout: LayoutTree,
        text: TextSystem,
        hit: HitTest,
        reg: OverlayRegistry,
        damage: Arc<DamageState>,
        theme: Theme,
    }

    fn harness(root: AnyElement) -> Harness {
        Harness {
            root,
            arena: Arena::new(),
            layout: LayoutTree::new(),
            text: TextSystem::new(FontDb::new()),
            hit: HitTest::new(),
            reg: OverlayRegistry::new(),
            damage: Arc::new(DamageState::default()),
            theme: Theme::light(),
        }
    }

    fn frame(h: &mut Harness) -> Scene {
        let mut scene = Scene::new();
        render_tree(
            &mut h.root,
            &mut h.arena,
            &mut h.layout,
            &mut h.text,
            None,
            Some(&mut h.hit),
            Some(&mut h.reg),
            None,
            None,
            None,
            &mut scene,
            VIEWPORT,
            &h.damage,
            &h.theme,
        )
        .unwrap();
        scene
    }

    /// The open popover's content quad (in the overlay band), if shown.
    fn content_quad(scene: &Scene) -> Option<&kagari_render::Quad> {
        scene
            .quads
            .iter()
            .find(|q| q.order >= OVERLAY_BAND && q.bg == content_bg())
    }

    #[test]
    fn popover_should_render_content_when_open() {
        let owner = Owner::new();
        owner.set();
        provide_context(OverlayLayers::new());

        let anchor = AnchorHandle::new();
        let open = RwSignal::new(false);
        let mut h = harness(popover(&anchor, open, content).into_element());

        // Closed: no content in the overlay band.
        assert!(
            content_quad(&frame(&mut h)).is_none(),
            "closed popover renders no content"
        );

        // Open: the content mounts (dyn_if) and paints in the overlay band, content-sized (60x30).
        open.set(true);
        let shown = frame(&mut h);
        let q = content_quad(&shown).expect("open popover renders its content");
        assert_eq!(
            q.bounds.size,
            BSize { w: 60.0, h: 30.0 },
            "the content is content-sized inside the overlay (not collapsed)"
        );

        // Closed again: the content unmounts.
        open.set(false);
        assert!(
            content_quad(&frame(&mut h)).is_none(),
            "toggling open false hides the content"
        );

        drop(owner);
    }

    #[test]
    fn popover_should_anchor_to_handle() {
        let owner = Owner::new();
        owner.set();
        provide_context(OverlayLayers::new());

        // A 40x20 anchor at the top-left; the popover is placed below it.
        let anchor = AnchorHandle::new();
        let open = RwSignal::new(true);
        let root = div()
            .flex_col()
            .child(
                div()
                    .size(BSize { w: 40.0, h: 20.0 })
                    .background(Background::Solid(Color::new(1.0, 0.0, 0.0, 1.0)))
                    .anchor_ref(&anchor),
            )
            .child(popover(&anchor, open, content))
            .into_element();
        let mut h = harness(root);

        let scene = frame(&mut h);
        let q = content_quad(&scene).expect("the open popover renders its content");
        assert_eq!(
            q.bounds.origin.y, 20.0,
            "Below places the popover at the anchor's bottom edge (y=20)"
        );
    }

    #[test]
    fn popover_outside_click_should_dismiss() {
        let owner = Owner::new();
        owner.set();
        let layers = OverlayLayers::new();
        provide_context(layers.clone());

        let anchor = AnchorHandle::new();
        let open = RwSignal::new(true);
        let mut h = harness(popover(&anchor, open, content).into_element());

        // Render: the open popover registers a dismiss-on-outside layer bound to `open`.
        assert!(
            content_quad(&frame(&mut h)).is_some(),
            "the popover is open"
        );
        assert!(
            !layers.is_empty(),
            "the open popover registered a dismiss layer"
        );

        // An outside press / Esc routes to `dismiss_topmost`, which clears the layer's `open` signal.
        assert!(
            layers.dismiss_topmost(),
            "the dismissible layer is dismissed"
        );
        assert!(
            !open.get_untracked(),
            "dismissing sets the controlled open signal false (#219)"
        );
        assert!(
            content_quad(&frame(&mut h)).is_none(),
            "the dismissed popover no longer renders its content"
        );

        drop(owner);
    }

    #[test]
    fn popover_close_should_return_focus() {
        // The interactive `.open()` path saves the focused element on open and restores it on close, so
        // closing the popover returns focus to the trigger (even if focus moved while it was open).
        let owner = Owner::new();
        owner.set();
        provide_context(OverlayLayers::new());
        let reg = FocusRegistry::new();
        provide_context(reg.clone());

        let trigger = reg.handle();
        let other = reg.handle();
        trigger.focus();

        let anchor = AnchorHandle::new();
        let open = RwSignal::new(false);
        let mut h = harness(popover(&anchor, open, content).into_element());
        frame(&mut h); // build → the overlay's open-state effect is registered (prev = false)

        // Open saves the focused trigger; then move focus elsewhere while open.
        open.set(true);
        other.focus();
        assert!(
            other.is_focused(),
            "focus moved elsewhere while the popover is open"
        );

        // Close returns focus to the pre-open trigger.
        open.set(false);
        assert_eq!(
            reg.current().get_untracked(),
            Some(trigger.id()),
            "closing the popover returns focus to the trigger"
        );

        drop(owner);
    }
}
