//! The [`Progress`] widget (#87): a determinate progress bar — an `Accent` fill over a `Border` track,
//! filling to a value in `0.0..=1.0`. The fill **animates** to its value via an `Animated<f32>` on the
//! #253 ticker (smooth catch-up when the value changes); a GPU-free test reaches the target on the next
//! manual `Ticker::tick_all` (like `scroll`/`switch`). The value is reactive (a signal / `rx`).

use kagari_base::Size;
use kagari_core::reactive::{Prop, create_effect, rx};
use kagari_core::{Animated, AnyElement, IntoElement, div};
use kagari_style::{ColorRole, Styled};

use crate::control::{CONTROL_SPRING, ControlSize, progress_dims};

/// A determinate progress bar (#87) bound to a `0.0..=1.0` value (static or reactive). Build with
/// [`progress`]; chain `.size(..)`. Returns `impl IntoElement`. The fill animates to the (clamped) value.
pub struct Progress {
    value: Prop<f32>,
    size: ControlSize,
}

/// Creates a medium progress bar filled to `value` (clamped to `0.0..=1.0`). `value` is static
/// (`progress(0.6)`) or reactive (`progress(rx(move || sig.get()))`).
pub fn progress(value: impl Into<Prop<f32>>) -> Progress {
    Progress {
        value: value.into(),
        size: ControlSize::Md,
    }
}

impl Progress {
    /// Sets the bar size on the shared control scale (D3) — a wider/taller track.
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }
}

impl IntoElement for Progress {
    fn into_element(self) -> AnyElement {
        let (track_w, track_h) = progress_dims(self.size);
        let (track_w, track_h) = (track_w.0, track_h.0);
        let clamp01 = |v: f32| v.clamp(0.0, 1.0);

        // The fill fraction animates to the (clamped) value. A static value fills 0 → value once; a
        // reactive value re-targets on change (the guard avoids a spurious start at the current value).
        let anim = Animated::new(0.0_f32, CONTROL_SPRING);
        match self.value {
            Prop::Static(v) => anim.set_target(clamp01(v)),
            Prop::Reactive(read) => {
                let retarget = anim.clone();
                create_effect(move || {
                    let target = clamp01(read());
                    if retarget.target() != target {
                        retarget.set_target(target);
                    }
                });
            }
        }

        // The fill is a reactive-width Accent pill; the track is a Border pill it sits in (left-aligned).
        let fill = div()
            .size(rx(move || Size {
                w: track_w * anim.get(),
                h: track_h,
            }))
            .bg(ColorRole::Accent)
            .rounded_full();
        div()
            .flex()
            .items_center()
            .size(Size {
                w: track_w,
                h: track_h,
            })
            .bg(ColorRole::Border)
            .rounded_full()
            .child(fill)
            .into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    use kagari_base::Size as BSize;
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::prelude::*;
    use kagari_core::reactive::{Owner, RwSignal, provide_context, rx};
    use kagari_core::{ActiveSources, Ticker};
    use kagari_layout::LayoutTree;
    use kagari_render::{Background, Scene};
    use kagari_style::{ColorRole, Theme};
    use kagari_text::{FontDb, TextSystem};

    const VIEWPORT: BSize = BSize { w: 400.0, h: 100.0 };

    /// Builds `progress(value)`, drives the ticker until the fill animation settles, and returns the
    /// laid-out width of the Accent fill quad.
    fn settled_fill_w(value: f32) -> f32 {
        let owner = Owner::new();
        owner.set();
        let ticker = Ticker::new();
        let active = ActiveSources::new();
        provide_context(ticker.clone());
        provide_context(active.clone());
        let theme = Theme::light();
        let accent = Background::Solid(theme.resolve_color(ColorRole::Accent));

        let mut root = progress(value).into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage = Arc::new(DamageState::default());

        // Drive the fill animation to rest (the active source releases on convergence).
        let mut ticks = 0;
        while active.count() > 0 && ticks < 2000 {
            ticker.tick_all(Duration::from_millis(16));
            ticks += 1;
        }
        let mut scene = Scene::new();
        render_tree(
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
            &theme,
        )
        .unwrap();
        let w = scene
            .quads
            .iter()
            .find(|q| q.bg == accent)
            .map_or(0.0, |q| q.bounds.size.w);
        drop(owner);
        w
    }

    #[test]
    fn progress_should_fill_to_value() {
        let track_w = crate::control::progress_dims(ControlSize::Md).0.0;

        let three_quarter = settled_fill_w(0.75);
        assert!(
            (three_quarter - 0.75 * track_w).abs() < 1.0,
            "the fill settles to 75% of the track: got {three_quarter}, want {}",
            0.75 * track_w
        );

        let over = settled_fill_w(1.5);
        assert!(
            (over - track_w).abs() < 1.0,
            "a value > 1 clamps to a full fill: got {over}, want {track_w}"
        );

        let empty = settled_fill_w(0.0);
        assert!(empty < 1.0, "a value of 0 is an ~empty fill: got {empty}");
    }

    #[test]
    fn progress_should_follow_reactive_value() {
        // A reactive value re-targets the fill: after a signal write the bar animates to the new value.
        let owner = Owner::new();
        owner.set();
        let ticker = Ticker::new();
        let active = ActiveSources::new();
        provide_context(ticker.clone());
        provide_context(active.clone());
        let theme = Theme::light();
        let accent = Background::Solid(theme.resolve_color(ColorRole::Accent));
        let track_w = crate::control::progress_dims(ControlSize::Md).0.0;

        let value = RwSignal::new(0.2_f32);
        let mut root = progress(rx(move || value.get())).into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage = Arc::new(DamageState::default());
        let settled_fill = |root: &mut AnyElement,
                            arena: &mut Arena,
                            layout: &mut LayoutTree,
                            text: &mut TextSystem| {
            let mut ticks = 0;
            while active.count() > 0 && ticks < 2000 {
                ticker.tick_all(Duration::from_millis(16));
                ticks += 1;
            }
            let mut scene = Scene::new();
            render_tree(
                root, arena, layout, text, None, None, None, None, None, None, &mut scene,
                VIEWPORT, &damage, &theme,
            )
            .unwrap();
            scene
                .quads
                .iter()
                .find(|q| q.bg == accent)
                .map_or(0.0, |q| q.bounds.size.w)
        };

        let at_20 = settled_fill(&mut root, &mut arena, &mut layout, &mut text);
        assert!(
            (at_20 - 0.2 * track_w).abs() < 1.0,
            "the initial fill settles to 20%: got {at_20}"
        );
        value.set(0.8);
        let at_80 = settled_fill(&mut root, &mut arena, &mut layout, &mut text);
        assert!(
            (at_80 - 0.8 * track_w).abs() < 1.0,
            "the signal write re-targets the fill to 80%: got {at_80}"
        );
        drop(owner);
    }
}
