//! The [`Label`] widget (#71): a thin, theme-aware wrapper over core [`text`] for
//! **display** text — no editing, not focusable. It defaults to the theme's `Text` role (so it follows a
//! theme swap, unlike a raw `text()` which uses a fixed light color), with `.color(..)` / `.muted()` for
//! the color role and `.size(..)` for the font size on the shared control scale (D3).
//!
//! **No wrapping yet**: available-width line wrapping needs a core `Text` change (measure-time shaping);
//! it is tracked in #260, which will add `Label::wrap(bool)` on top of this.

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
}

/// Creates a label showing `content` in the theme's `Text` color at the medium size.
pub fn label(content: impl Into<SharedString>) -> Label {
    Label {
        content: content.into(),
        color: ColorRole::Text,
        size: ControlSize::Md,
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
}

impl IntoElement for Label {
    fn into_element(self) -> AnyElement {
        // Wrap the glyph text in a `Role::Label` node so the label surfaces in the accesskit tree (a
        // bare `text()` leaf carries no a11y — kagari's a11y is Div-based, #257 / RK-032). The wrapper
        // sizes to the text, so layout is unchanged.
        div()
            .role(Role::Label)
            .a11y_label(self.content.clone())
            .child(
                text(self.content)
                    .color_role(self.color)
                    .size(label_px(self.size)),
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

    /// Lays out `label` alone and returns the root text node's computed size.
    fn measured(label: Label) -> BSize {
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
            VIEWPORT,
            &damage,
            &Theme::light(),
        )
        .unwrap();
        layout.layout(root_id).size
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
}
