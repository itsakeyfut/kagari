//! A shared reactive-prop → paint-slot binding (#245/#251).
//!
//! The pattern "resolve a [`Prop<T>`] into an `Arc<Mutex<Option<T>>>` slot — a static value once, a
//! reactive closure via a synchronous effect that flags **paint**-dirty only on a real change — and read
//! it back at paint" was duplicated across `Div` (background / bg-role / border-role / a11y-checked),
//! `Text` (color-role), and `Icon` (tint). [`ReactiveProp`] is the single home for it, so a change to the
//! §9 damage discipline is one edit, not six.
//!
//! Not used for `Div::size`: that flags **layout**-dirty (a size change needs relayout) and is entangled
//! with the layout-token dedup, so it keeps its own binding.

use std::sync::{Arc, Mutex};

use kagari_base::NodeId;

use crate::element::DamageSink;
use crate::reactive::{Prop, create_effect};

/// A [`Prop<T>`] staged at build and resolved into a paint-time slot. Holds the unresolved prop plus the
/// shared resolved cell; `paint` reads the resolved value via [`current`](Self::current).
pub(crate) struct ReactiveProp<T> {
    prop: Option<Prop<T>>,
    resolved: Arc<Mutex<Option<T>>>,
}

impl<T> Default for ReactiveProp<T> {
    fn default() -> Self {
        Self {
            prop: None,
            resolved: Arc::new(Mutex::new(None)),
        }
    }
}

impl<T: Copy + PartialEq + Send + Sync + 'static> ReactiveProp<T> {
    /// A prop pre-staged with `prop` — e.g. a default role set at construction.
    pub(crate) fn new(prop: impl Into<Prop<T>>) -> Self {
        let mut this = Self::default();
        this.set(prop);
        this
    }

    /// Stages a prop (static or reactive) — resolved later by [`bind`](Self::bind). Called by a builder.
    pub(crate) fn set(&mut self, prop: impl Into<Prop<T>>) {
        self.prop = Some(prop.into());
    }

    /// Clears any staged prop (e.g. a raw color overriding a role) — resolves to `None`.
    pub(crate) fn clear(&mut self) {
        self.prop = None;
    }

    /// Resolves the staged prop at build: a static value writes the slot once; a reactive closure
    /// registers a synchronous effect (ADR 0001) that re-resolves and, **only when the value changed**
    /// (design.md §9 — a signal re-runs subscribers even on an equal write, and idle repaints violate
    /// the damage discipline), writes the slot and flags paint-dirty for `id`. A no-op if nothing was set.
    pub(crate) fn bind(&mut self, damage: &Arc<dyn DamageSink>, id: NodeId) {
        match self.prop.take() {
            Some(Prop::Static(value)) => {
                if let Ok(mut slot) = self.resolved.lock() {
                    *slot = Some(value);
                }
            }
            Some(Prop::Reactive(read)) => {
                let cell = Arc::clone(&self.resolved);
                let damage = Arc::clone(damage);
                create_effect(move || {
                    let value = read();
                    if let Ok(mut slot) = cell.lock() {
                        if *slot != Some(value) {
                            *slot = Some(value);
                            damage.mark_paint_dirty(id);
                        }
                    }
                });
            }
            None => {}
        }
    }

    /// The value resolved this frame (`None` = never resolved / no prop set).
    pub(crate) fn current(&self) -> Option<T> {
        self.resolved.lock().ok().and_then(|slot| *slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::damage::DamageState;
    use crate::reactive::prelude::*;
    use crate::reactive::{Owner, RwSignal, rx};

    #[test]
    fn reactive_prop_should_flag_dirty_only_on_change() {
        // The core §9 invariant: a reactive prop flags paint-dirty on a real change, not on an
        // equal-value re-resolve. RK-005: serial + owner alive.
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(1_i32);
        let damage = Arc::new(DamageState::default());
        let sink: Arc<dyn DamageSink> = damage.clone();
        let id = NodeId::from_raw(1);

        let mut prop: ReactiveProp<i32> = ReactiveProp::default();
        prop.set(rx(move || value.get()));
        prop.bind(&sink, id);
        assert_eq!(prop.current(), Some(1), "resolves its initial value");

        damage.clear();
        value.set(1); // same value
        assert!(
            !damage.is_dirty(),
            "re-resolving the same value flags no damage"
        );

        value.set(2); // changed
        assert!(damage.is_dirty(), "a changed value flags paint-dirty");
        assert_eq!(prop.current(), Some(2), "the slot tracks the new value");
        drop(owner);
    }

    #[test]
    fn reactive_prop_static_should_resolve_without_damage() {
        let owner = Owner::new();
        owner.set();
        let damage = Arc::new(DamageState::default());
        let sink: Arc<dyn DamageSink> = damage.clone();

        let mut prop: ReactiveProp<i32> = ReactiveProp::default();
        prop.set(7); // static
        prop.bind(&sink, NodeId::from_raw(1));
        assert_eq!(prop.current(), Some(7), "a static prop resolves once");
        assert!(!damage.is_dirty(), "a static prop flags no damage");
        drop(owner);
    }
}
