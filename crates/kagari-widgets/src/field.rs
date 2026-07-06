//! The [`Field`] widget (#236): a labeled form/inspector row — a label, an arbitrary control, and an
//! optional help / live error message, for consistent settings rows across the editors.
//!
//! `Field` is a **pure layout + semantics wrapper** with no focus of its own — the inner control focuses.
//! The control is an opaque `impl IntoElement`, so `Field` cannot reach into its border; the error state
//! is shown by a reactive `Danger` border on `Field`'s own control-slot wrapper (a no-padding inset that
//! reads as the control's border turning red) plus a `Danger` message. The message is a single reactive
//! line: the error (if the bound signal is `Some`), otherwise the help text, otherwise empty — updated
//! live via reactive [`text`](kagari_core::text) content (#274).

use kagari_base::SharedString;
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{AnyElement, IntoElement, div, text};
use kagari_style::{ColorRole, Styled};

use crate::control::{ControlSize, label_px};
use crate::label::label;

/// How a [`Field`] arranges its label and control. `Row` (the default) is a label-left inspector row;
/// `Column` stacks the label above the control (a denser / narrow-panel form).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum FieldLayout {
    /// Label left, control (and message) to the right — the default inspector row.
    #[default]
    Row,
    /// Label above the control — a stacked form field.
    Column,
}

/// A labeled field wrapping a control (#236). Build with [`field`]; chain `.help(..)` / `.error(..)` /
/// `.layout(..)` / `.size(..)`. Has no focus of its own — the inner control focuses. A bound `.error`
/// signal reactively tints the control slot with a `Danger` border and shows its message.
pub struct Field {
    label: SharedString,
    control: AnyElement,
    help: Option<SharedString>,
    error: Option<RwSignal<Option<SharedString>>>,
    layout: FieldLayout,
    size: ControlSize,
}

/// Creates a field labeling `control` with `label` (a `Row` inspector row at the medium size).
pub fn field(label: impl Into<SharedString>, control: impl IntoElement) -> Field {
    Field {
        label: label.into(),
        control: control.into_element(),
        help: None,
        error: None,
        layout: FieldLayout::Row,
        size: ControlSize::Md,
    }
}

impl Field {
    /// Adds static help text shown (muted) below the control when there is no active error.
    pub fn help(mut self, help: impl Into<SharedString>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// Binds a reactive error: when the signal is `Some(msg)`, the control slot gains a `Danger` border
    /// and the message line shows `msg` in `Danger`; when `None`, the border is hidden and the help (if
    /// any) is shown instead. Controlled (D2) — the caller owns the signal.
    pub fn error(mut self, error: RwSignal<Option<SharedString>>) -> Self {
        self.error = Some(error);
        self
    }

    /// Sets the label/control arrangement ([`FieldLayout::Row`] by default).
    pub fn layout(mut self, layout: FieldLayout) -> Self {
        self.layout = layout;
        self
    }

    /// Sets the control size (D3) — drives the label and message font size.
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }
}

impl IntoElement for Field {
    fn into_element(self) -> AnyElement {
        let Field {
            label: title,
            control,
            help,
            error,
            layout,
            size,
        } = self;

        // Label — reuse the Label widget so it surfaces in the accesskit tree (`Role::Label` + a11y_label).
        let label_el = label(title).size(size);

        // Control slot: a `Danger` border ringing the control when the bound error is present. The control
        // is opaque, so Field tints its own wrapper. A small always-on padding gives the ring breathing
        // room *around* the control (a flush border would overlap the control's own edge and be hard to
        // see), and — since the border draws only when a color is present ("costs no layout") — the
        // padding-not-the-border keeps the layout stable, so toggling the error shifts nothing.
        let mut slot = div().rounded_md().p_1().child(control);
        if let Some(err) = error {
            slot = slot
                .border_w_2()
                .border_color(rx(move || err.get().is_some().then_some(ColorRole::Danger)));
        }

        // Message: a single reactive line — the error (Danger) if the signal is `Some`, else the help
        // (muted), else empty. Present only when help or an error signal is configured. Live via #274.
        let message = (help.is_some() || error.is_some()).then(|| {
            let help_c = help.clone();
            let content = rx(move || {
                if let Some(err) = error {
                    if let Some(msg) = err.get() {
                        return msg;
                    }
                }
                help_c.clone().unwrap_or_else(|| SharedString::from(""))
            });
            let color = rx(move || {
                if error.map(|e| e.get().is_some()).unwrap_or(false) {
                    ColorRole::Danger
                } else {
                    ColorRole::TextMuted
                }
            });
            text(content).color_role(color).size(label_px(size))
        });

        // `items_start` on the slot's column so the control slot (and its Danger border) hugs the
        // control's own width instead of stretching to the column's widest child (e.g. a long message) —
        // flex defaults to `align-items: stretch`, which otherwise blows the error border out to the
        // message width.
        match layout {
            FieldLayout::Row => {
                // Label left; the control and its message stacked on the right.
                let mut right = div().flex_col().items_start().gap_1().child(slot);
                if let Some(m) = message {
                    right = right.child(m);
                }
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(label_el)
                    .child(right)
                    .into_element()
            }
            FieldLayout::Column => {
                let mut col = div()
                    .flex_col()
                    .items_start()
                    .gap_1()
                    .child(label_el)
                    .child(slot);
                if let Some(m) = message {
                    col = col.child(m);
                }
                col.into_element()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use kagari_base::Size as BSize;
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::div;
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::Owner;
    use kagari_layout::LayoutTree;
    use kagari_render::Scene;
    use kagari_style::{ColorRole, Theme};
    use kagari_text::{FontDb, TextSystem};

    const VIEWPORT: BSize = BSize { w: 300.0, h: 200.0 };

    /// A simple fixed-size stand-in control (deterministic bounds, no reactive effects of its own).
    fn control() -> impl IntoElement {
        div().size(BSize { w: 40.0, h: 20.0 })
    }

    struct Built {
        arena: Arena,
        layout: LayoutTree,
        root_id: kagari_base::NodeId,
    }

    fn build(widget: impl IntoElement, theme: &Theme) -> Built {
        let mut root = widget.into_element();
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
            theme,
        )
        .unwrap();
        Built {
            arena,
            layout,
            root_id,
        }
    }

    #[test]
    fn field_should_layout_label_and_control() {
        // A Row field lays the label to the left of the control area, and both nodes exist.
        let owner = Owner::new();
        owner.set();
        let b = build(field("Name", control()), &Theme::light());
        let kids = b
            .arena
            .get(b.root_id)
            .map(|n| n.children.clone())
            .unwrap_or_default();
        assert!(
            kids.len() >= 2,
            "a field has a label and a control area: {kids:?}"
        );
        let label_b = b.layout.layout(kids[0]);
        let control_b = b.layout.layout(kids[1]);
        assert!(
            label_b.origin.x < control_b.origin.x,
            "Row: the label sits left of the control: label={label_b:?} control={control_b:?}"
        );
        drop(owner);
    }

    #[test]
    fn field_column_should_stack() {
        // A Column field lays the label above the control.
        let owner = Owner::new();
        owner.set();
        let b = build(
            field("Name", control()).layout(FieldLayout::Column),
            &Theme::light(),
        );
        let kids = b
            .arena
            .get(b.root_id)
            .map(|n| n.children.clone())
            .unwrap_or_default();
        assert!(kids.len() >= 2, "label + control: {kids:?}");
        let label_b = b.layout.layout(kids[0]);
        let control_b = b.layout.layout(kids[1]);
        assert!(
            label_b.origin.y < control_b.origin.y,
            "Column: the label sits above the control: label={label_b:?} control={control_b:?}"
        );
        drop(owner);
    }

    #[test]
    fn field_error_should_show_message() {
        // A bound error signal reactively tints the control slot with a Danger border: absent when the
        // signal is None, present (live) once it becomes Some.
        let owner = Owner::new();
        owner.set();
        let theme = Theme::light();
        let danger = theme.resolve_color(ColorRole::Danger);
        let err = RwSignal::new(None::<SharedString>);

        let mut root = field("Name", control()).error(err).into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage = Arc::new(DamageState::default());
        let render = |root: &mut AnyElement,
                      arena: &mut Arena,
                      layout: &mut LayoutTree,
                      text: &mut TextSystem|
         -> Scene {
            let mut scene = Scene::new();
            render_tree(
                root, arena, layout, text, None, None, None, None, None, None, &mut scene,
                VIEWPORT, &damage, &theme,
            )
            .unwrap();
            scene
        };
        let has_danger_border = |s: &Scene| {
            s.quads
                .iter()
                .any(|q| q.border.color == danger && q.border.widths.top > 0.0)
        };

        let scene = render(&mut root, &mut arena, &mut layout, &mut text);
        assert!(
            !has_danger_border(&scene),
            "no error → no Danger border on the control slot"
        );

        err.set(Some("Required".into()));
        let scene = render(&mut root, &mut arena, &mut layout, &mut text);
        assert!(
            has_danger_border(&scene),
            "an active error tints the control slot with a Danger border"
        );
        drop(owner);
    }
}
