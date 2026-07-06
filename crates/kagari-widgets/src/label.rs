//! The [`Label`] widget (#71): a thin, theme-aware wrapper over core [`text`] for
//! **display** text — no editing, not focusable. It defaults to the theme's `Text` role (so it follows a
//! theme swap, unlike a raw `text()` which uses a fixed light color), with `.color(..)` / `.muted()` for
//! the color role and `.size(..)` for the font size on the shared control scale (D3).
//!
//! **Wrapping (#260)**: `.wrap(true)` line-breaks the label to its available width (via core `Text`'s
//! measure-time shaping); off by default, so a label stays single-line unless asked to wrap.

use kagari_base::SharedString;
use kagari_core::{AnyElement, IntoElement, Role, div, text};
use kagari_style::ColorRole;

use crate::control::{ControlSize, label_px};

/// A display-text label (#71). Build with [`label`]; chain `.color(..)` / `.muted()` / `.size(..)`.
/// Returns `impl IntoElement`. For editable text use `TextInput` (#72); for a button-shaped control use
/// [`Button`](crate::Button).
pub struct Label {
    content: SharedString,
    color: ColorRole,
    size: ControlSize,
    wrap: bool,
}

/// Creates a label showing `content` in the theme's `Text` color at the medium size.
pub fn label(content: impl Into<SharedString>) -> Label {
    Label {
        content: content.into(),
        color: ColorRole::Text,
        size: ControlSize::Md,
        wrap: false,
    }
}

impl Label {
    /// Sets the text color from a semantic role (theme-resolved at paint).
    pub fn color(mut self, role: ColorRole) -> Self {
        self.color = role;
        self
    }

    /// Muted (secondary) text — shorthand for `.color(ColorRole::TextMuted)`.
    pub fn muted(mut self) -> Self {
        self.color = ColorRole::TextMuted;
        self
    }

    /// Sets the font size via the shared control scale (D3): `Sm`/`Md`/`Lg` → 14/16/18 px.
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }

    /// Wraps the label to its available width (#260): line-breaks to the container width, staying
    /// single-line when the width is unconstrained. Off by default.
    pub fn wrap(mut self, wrap: bool) -> Self {
        self.wrap = wrap;
        self
    }
}

impl IntoElement for Label {
    fn into_element(self) -> AnyElement {
        // Wrap the glyph text in a `Role::Label` node so the label surfaces in the accesskit tree (a
        // bare `text()` leaf carries no a11y — kagari's a11y is Div-based, #257 / RK-032). The wrapper
        // sizes to the text, so layout is unchanged.
        let mut wrapper = div().role(Role::Label).a11y_label(self.content.clone());
        // A wrapping label must let its wrapper shrink below its content too (`min-width: 0`), or the
        // flex wrapper keeps the text at its single-line width and it never wraps (#260).
        if self.wrap {
            wrapper = wrapper.min_w(0.0);
        }
        wrapper
            .child(
                text(self.content)
                    .color_role(self.color)
                    .size(label_px(self.size))
                    .wrap(self.wrap),
            )
            .into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use kagari_base::Size as BSize;
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::paint::render_tree;
    use kagari_layout::LayoutTree;
    use kagari_render::Scene;
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};

    // Wide viewport so the (short) label text is never width-clamped — its laid-out size is the shaped
    // intrinsic size. (Glyph color needs an atlas, so it is not observable in these GPU-free tests; the
    // color role forwards to core `text().color_role`, covered by kagari-core's text color tests.)
    const VIEWPORT: BSize = BSize {
        w: 1000.0,
        h: 200.0,
    };

    /// Lays out `label` alone at `viewport` and returns the root node's computed size.
    fn measured_at(label: Label, viewport: BSize) -> BSize {
        let mut root = label.into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let mut scene = Scene::new();
        let damage = Arc::new(DamageState::default());
        let root_id = render_tree(
            &mut root,
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
            viewport,
            &damage,
            &Theme::light(),
        )
        .unwrap();
        layout.layout(root_id).size
    }

    /// Lays out `label` at the wide default viewport (text never width-clamped).
    fn measured(label: Label) -> BSize {
        measured_at(label, VIEWPORT)
    }

    #[test]
    fn label_size_should_scale_with_control_size() {
        // A larger control size maps to a larger font, so the same text lays out bigger.
        let sm = measured(label("Hello, kagari").size(ControlSize::Sm));
        let lg = measured(label("Hello, kagari").size(ControlSize::Lg));
        assert!(
            lg.w > sm.w && lg.h > sm.h,
            "Lg text should be wider and taller than Sm: lg={lg:?} sm={sm:?}"
        );
    }

    #[test]
    fn label_should_size_to_its_text_content() {
        // The content flows through to shaping: longer text lays out wider (same size).
        let short = measured(label("Hi"));
        let long = measured(label("Hi there, this is a longer label"));
        assert!(
            long.w > short.w,
            "longer content should be wider: long={long:?} short={short:?}"
        );
    }

    /// Lays out `label` inside a fixed-width (`parent_w` × tall) flex parent whose cross-axis is not
    /// stretched (`items_start`), so the label sizes to its wrapped content, and returns the label's
    /// computed size. (A label as the *root* would size a flex container to its content and never see a
    /// width constraint; the realistic case is a width-constrained parent — #260.)
    fn measured_in_width(label: Label, parent_w: f32) -> BSize {
        let mut root = div()
            .size(BSize {
                w: parent_w,
                h: 400.0,
            })
            .items_start()
            .child(label)
            .into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let mut scene = Scene::new();
        let damage = Arc::new(DamageState::default());
        let root_id = render_tree(
            &mut root,
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
            BSize { w: 400.0, h: 800.0 },
            &damage,
            &Theme::light(),
        )
        .unwrap();
        let label_id = arena
            .get(root_id)
            .and_then(|n| n.children.first().copied())
            .expect("the parent has the label as its child");
        layout.layout(label_id).size
    }

    #[test]
    fn label_wrap_should_break_lines() {
        // In a fixed-width parent, a wrapping label breaks its (long) text to multiple lines — laying out
        // taller and within the parent width — while a non-wrapping one stays single-line, overflowing (#260).
        let long = "The quick brown fox jumps over the lazy dog";
        let wrapped = measured_in_width(label(long).wrap(true), 80.0);
        let single = measured_in_width(label(long).wrap(false), 80.0);
        assert!(
            wrapped.h > single.h,
            "a wrapped label is taller (multi-line) than a single-line one: wrapped={wrapped:?} single={single:?}"
        );
        assert!(
            wrapped.w <= 80.5 && single.w > 80.5,
            "the wrapped label fits the parent width while the single-line one overflows: wrapped={wrapped:?} single={single:?}"
        );
    }
}
