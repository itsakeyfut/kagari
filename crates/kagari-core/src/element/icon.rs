//! The [`Icon`] leaf element (#246): a composable bundled [`IconId`] that widgets place like a
//! [`text`](super::text) label. It emits a deferred [`IconSprite`] into the scene; the renderer
//! rasterizes it at the resolved physical size (crisp) and draws it tinted. Monochrome bundled icons
//! are tintable by a semantic role (theme-reactive, #245) or a raw color.

use kagari_base::{Color, Corners, NodeId, Px, Rect, Size};
use kagari_layout::LayoutStyle;
use kagari_render::{IconId, IconSprite, RoundedRect};
use kagari_style::ColorRole;

use super::reactive_prop::ReactiveProp;
use super::{AnyElement, Element, Event, EventCx, IntoElement, LayoutCx, PaintCx};
use crate::arena::Node;
use crate::reactive::Prop;

/// An icon leaf: a bundled [`IconId`] drawn at a square logical size, tinted by a semantic role (the
/// default, [`ColorRole::Text`]) or a raw color. Build with [`icon`]; chain `.size(..)`,
/// `.color_role(..)`, or `.color(..)`.
pub struct Icon {
    icon: IconId,
    size: Px,
    /// Raw tint fallback; used when no `color_role` is set. Multiplies the monochrome-white icon.
    color: Color,
    /// Semantic tint **role** (static or reactive, #245), resolved to a color at paint via `cx.theme`.
    /// Takes precedence over `color`. Defaults to [`ColorRole::Text`] so an icon follows the text color.
    color_role: ReactiveProp<ColorRole>,
    id: Option<NodeId>,
}

/// Creates an icon leaf for `icon` at the default size (16px), tinted with [`ColorRole::Text`].
pub fn icon(icon: IconId) -> Icon {
    Icon {
        icon,
        size: Px(16.0),
        color: Color::new(1.0, 1.0, 1.0, 1.0),
        color_role: ReactiveProp::new(ColorRole::Text),
        id: None,
    }
}

impl Icon {
    /// Sets the square logical size (both width and height).
    pub fn size(mut self, size: Px) -> Self {
        self.size = size;
        self
    }

    /// Tints the icon with a semantic **role** — a static token or a reactive [`Prop`] (#245), resolved
    /// at paint via `cx.theme` (so it follows a theme swap / state signal). Takes precedence over
    /// [`color`](Self::color).
    pub fn color_role(mut self, role: impl Into<Prop<ColorRole>>) -> Self {
        self.color_role.set(role);
        self
    }

    /// Tints the icon with a raw [`Color`], clearing any role tint (raw then wins at paint).
    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self.color_role.clear();
        self
    }
}

impl Element for Icon {
    fn request_layout(&mut self, cx: &mut LayoutCx) -> NodeId {
        if let Some(id) = self.id {
            return id;
        }
        let id = cx.arena.insert(Node::default());
        self.id = Some(id);
        cx.layout.insert(id, None);
        self.color_role.bind(&cx.damage, id);

        // A fixed square measure — the icon draws at the requested logical size regardless of content.
        let size = Size {
            w: self.size.0,
            h: self.size.0,
        };
        cx.layout.set_measure(id, Box::new(move |_avail| size));
        cx.layout.set_style(id, &LayoutStyle::default());
        id
    }

    fn paint(&mut self, bounds: Rect, cx: &mut PaintCx) {
        // Resolve the tint: a role (via `cx.theme`, so it follows a theme swap / state signal) takes
        // precedence over the raw color. Emit a deferred icon request — the renderer rasterizes it at the
        // physical size and draws it; no atlas is touched here (so GPU-free tests still see the request).
        let tint = self
            .color_role
            .current()
            .map(|role| cx.theme.resolve_color(role))
            .unwrap_or(self.color);
        let order = cx.next_order();
        let mask = cx.clip_rect(bounds);
        cx.scene.icons.push(IconSprite {
            bounds,
            icon: self.icon,
            tint,
            content_mask: RoundedRect {
                rect: mask,
                radii: Corners::default(),
            },
            order,
        });
    }

    fn handle_event(&mut self, _ev: &Event, _cx: &mut EventCx) {}
}

impl IntoElement for Icon {
    fn into_element(self) -> AnyElement {
        Box::new(self)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::arena::Arena;
    use crate::damage::DamageState;
    use crate::paint::render_tree;
    use crate::reactive::rx;
    use kagari_layout::LayoutTree;
    use kagari_render::Scene;
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};
    use reactive_graph::owner::Owner;
    use reactive_graph::prelude::*;
    use reactive_graph::signal::signal;

    const VIEWPORT: Size = Size { w: 100.0, h: 100.0 };

    /// Builds `root` through the paint pass with **no atlas** and returns the scene. The deferred icon
    /// model emits its request into `scene.icons` regardless of an atlas (the renderer rasterizes it
    /// later), so a GPU-free test can assert the request — unlike a glyph, which needs an atlas.
    fn render_once(root: &mut AnyElement, theme: &Theme) -> Scene {
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage = Arc::new(DamageState::default());
        let mut scene = Scene::new();
        render_tree(
            root,
            &mut arena,
            &mut layout,
            &mut text,
            None,
            None,
            None,
            None,
            None,
            None,
            &mut scene,
            VIEWPORT,
            &damage,
            theme,
        )
        .unwrap();
        scene
    }

    #[test]
    fn icon_should_emit_icon_sprite_with_role_tint() {
        let owner = Owner::new();
        owner.set();
        let theme = Theme::light();
        let mut root = icon(IconId::Check).size(Px(20.0)).into_element();
        let scene = render_once(&mut root, &theme);

        assert_eq!(scene.icons.len(), 1, "the icon emits one deferred request");
        let s = scene.icons[0];
        assert_eq!(s.icon, IconId::Check);
        assert_eq!(
            s.tint,
            theme.resolve_color(ColorRole::Text),
            "the default tint is the Text role"
        );
        assert_eq!(s.bounds.size.w, 20.0);
        assert_eq!(s.bounds.size.h, 20.0);
        drop(owner);
    }

    #[test]
    fn icon_color_should_override_role_tint() {
        let theme = Theme::light();
        let raw = Color::new(1.0, 0.0, 0.0, 1.0);
        let mut root = icon(IconId::Close).color(raw).into_element();
        let scene = render_once(&mut root, &theme);
        assert_eq!(
            scene.icons[0].tint, raw,
            "a raw .color() overrides the default role tint"
        );
    }

    #[test]
    fn icon_reactive_tint_should_update_on_signal() {
        let owner = Owner::new();
        owner.set();
        let theme = Theme::light();
        let (read, write) = signal(false);
        let mut root = icon(IconId::Check)
            .color_role(rx(move || {
                if read.get() {
                    ColorRole::Danger
                } else {
                    ColorRole::Text
                }
            }))
            .into_element();

        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage = Arc::new(DamageState::default());
        let frame = |root: &mut AnyElement,
                     arena: &mut Arena,
                     layout: &mut LayoutTree,
                     text: &mut TextSystem| {
            let mut scene = Scene::new();
            render_tree(
                root, arena, layout, text, None, None, None, None, None, None, &mut scene,
                VIEWPORT, &damage, &theme,
            )
            .unwrap();
            scene
        };

        let scene = frame(&mut root, &mut arena, &mut layout, &mut text);
        assert_eq!(scene.icons[0].tint, theme.resolve_color(ColorRole::Text));

        write.set(true);
        assert!(
            damage.is_dirty(),
            "the reactive tint write flags paint damage"
        );

        let scene = frame(&mut root, &mut arena, &mut layout, &mut text);
        assert_eq!(
            scene.icons[0].tint,
            theme.resolve_color(ColorRole::Danger),
            "the signal write re-resolves the tint role"
        );
        drop(owner);
    }
}
