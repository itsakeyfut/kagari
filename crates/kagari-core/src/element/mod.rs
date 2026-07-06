//! Element tree (#31): the [`Element`] trait, opaque [`IntoElement`], [`AnyElement`], and the
//! [`Div`] container.
//!
//! Public authoring is gpui-style (a builder chain returning an opaque element); the retained
//! tree lives in the arena (#30), keyed by `NodeId`, per spec §1.3 (Floem-style id-keyed nodes).
//! The Tailwind token API, the `Styled`-trait-on-elements-only encapsulation, and `impl
//! IntoElement` component encapsulation are deferred to the styling milestone (/spec Q4); Phase 3
//! uses raw `kagari-render`/`kagari-base` values and exposes only `Div`.

mod cx;
mod div;
mod dynamic;
mod icon;
mod overlay;
mod reactive_prop;
mod scroll;
mod text;
mod transform;
mod virtualized;

pub use cx::{DamageSink, Event, EventCx, LayoutCx, PaintCx};
pub use div::{Div, div};
pub use dynamic::{DynIf, DynList, dyn_if, dyn_list};
pub use icon::{Icon, icon};
pub use overlay::{AnchorHandle, Overlay, Placement, overlay};
pub use scroll::{Scroll, ScrollHandle, scroll};
pub use text::{Text, text};
pub use transform::{TransformOrigin, Transformed, transform};
pub use virtualized::{VirtualizedList, virtualized_list};

use kagari_base::{NodeId, Rect};

/// A retained UI element: it builds its node into the arena, paints primitives into the scene,
/// and handles events. Object-safe so elements compose as [`AnyElement`].
pub trait Element {
    /// Build/register this element's node in the arena and return its id, recursing into
    /// children. #33 adds taffy style registration here.
    fn request_layout(&mut self, cx: &mut LayoutCx) -> NodeId;

    /// Emit this element's primitives into the scene for the given laid-out `bounds`. Child
    /// traversal is driven by the paint pass (#34).
    fn paint(&mut self, bounds: Rect, cx: &mut PaintCx);

    /// Handle an input event. Minimal seam for Phase 3 (no events exist yet).
    fn handle_event(&mut self, ev: &Event, cx: &mut EventCx);

    /// Apply any staged structural change and recurse into children — the frame loop's reconcile pass
    /// (#278). Runs after `request_layout`, before `compute` (a second, gated *build* pass — hence
    /// `LayoutCx`, not `PaintCx`). The default is a no-op, correct for **leaves** only.
    ///
    /// **Contract: every container element MUST override this to recurse into its children** (mirroring
    /// its `paint`/`request_layout` child recursion), or a nested `dyn_if`/`dyn_list` beneath it will not
    /// reconcile live. A `dyn` container additionally applies its own staged mount/unmount first, then
    /// recurses. This obligation is not compile-enforced (the trait has no children accessor), so the
    /// crate test `containers_should_recurse_reconcile_into_nested_dyn` guards it.
    fn reconcile(&mut self, _cx: &mut LayoutCx) {}
}

/// Conversion into an opaque [`AnyElement`], letting builder methods accept `impl IntoElement`.
pub trait IntoElement {
    fn into_element(self) -> AnyElement;
}

/// A type-erased element.
pub type AnyElement = Box<dyn Element>;

impl IntoElement for AnyElement {
    fn into_element(self) -> AnyElement {
        self
    }
}
