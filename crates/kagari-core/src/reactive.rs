//! Reactive runtime wrapper (#29).
//!
//! Thin kagari-core seam over `reactive_graph` (adopted in #28, ADR 0001): fine-grained
//! `signal`/`memo`/`effect` plus owner-scoped `context` (provide/use), and the [`Prop`]
//! reactive-or-static prop type that elements (#31) bind. The runtime stays an
//! implementation detail behind these wrappers; the only public reactive surface is what
//! this module re-exports.
//!
//! Effects use the **synchronous** `ImmediateEffect` (ADR 0001): a signal write re-runs the
//! subscribed effect on the spot and marks the element dirty (spec §1.4). [`create_effect`]
//! is owner-scoped, so an effect lives until the current owner is disposed — callers hold no
//! guard. Context uses `reactive_graph`'s owner-scoped store, so there is **no hidden global**
//! (`static`/`thread_local!`) of our own (design.md §3).

pub use reactive_graph::computed::Memo;
pub use reactive_graph::owner::Owner;
pub use reactive_graph::signal::{ReadSignal, RwSignal, WriteSignal};

/// Reactive traits (`Get`/`Set`/…) needed to read and write the signals/memos this module
/// returns. Re-exported from `reactive_graph` so callers do not depend on it directly.
pub mod prelude {
    pub use reactive_graph::prelude::*;
}

/// Creates a fine-grained signal, returning its read and write halves.
///
/// Reads registered during an effect/build subscribe that scope to this signal; a write
/// re-runs the subscribers (synchronously, see [`create_effect`]).
pub fn create_signal<T: Send + Sync + 'static>(value: T) -> (ReadSignal<T>, WriteSignal<T>) {
    reactive_graph::signal::signal(value)
}

/// Creates a memoized derived value that recomputes only when a tracked dependency changes
/// and its result differs (hence the `PartialEq` bound).
pub fn create_memo<T: PartialEq + Send + Sync + 'static>(
    f: impl Fn() -> T + Send + Sync + 'static,
) -> Memo<T> {
    // `reactive_graph` passes the previous value for incremental memos; this wrapper does not
    // expose that, so the recompute closure ignores it.
    Memo::new(move |_prev| f())
}

/// Registers an effect that runs immediately and again whenever a tracked signal it read
/// changes. The effect is scoped to the current reactive owner and is cleaned up when that
/// owner is disposed, so the caller keeps no handle.
///
/// `Fn + Send + Sync` (not `FnMut`) because the synchronous effect may recurse and its body is
/// stored shareably; update node state through interior mutability / signals, not `&mut`.
pub fn create_effect(f: impl Fn() + Send + Sync + 'static) {
    reactive_graph::effect::ImmediateEffect::new_scoped(f);
}

/// Provides a value into the current reactive owner's context, keyed by its type. Consumed by
/// [`use_context`] in this owner or any descendant scope. Used for theme/services/stores
/// (specs §1.8) — no hidden global.
pub fn provide_context<T: Send + Sync + 'static>(value: T) {
    reactive_graph::owner::provide_context(value);
}

/// Resolves a value previously provided via [`provide_context`] in the current owner or an
/// ancestor scope, or `None` if absent.
pub fn use_context<T: Clone + 'static>() -> Option<T> {
    reactive_graph::owner::use_context()
}

/// A prop that is either a static value or a reactive closure.
///
/// Most props are [`Prop::Static`] and hold their value inline (no runtime reactive node). A
/// [`Prop::Reactive`] is bound by the element (#31) via an effect that re-reads the closure,
/// updates the node field, and flags damage (#35). Builder methods take `impl Into<Prop<T>>`,
/// keeping `reactive_graph` types out of the public builder surface.
///
/// The reactive closure is `Send + Sync` so it can be moved into the synchronous effect that
/// binds it (ADR 0001); mirrors leptos `MaybeSignal` over `SyncStorage`.
pub enum Prop<T> {
    Static(T),
    Reactive(Box<dyn Fn() -> T + Send + Sync + 'static>),
}

impl<T> From<T> for Prop<T> {
    fn from(value: T) -> Self {
        Prop::Static(value)
    }
}

/// Constructs a reactive [`Prop`] from a closure (the reactive counterpart of `T::into()`,
/// which yields a [`Prop::Static`]).
pub fn rx<T>(f: impl Fn() -> T + Send + Sync + 'static) -> Prop<T> {
    Prop::Reactive(Box::new(f))
}

impl<T: Clone> Prop<T> {
    /// Evaluates the prop: clones the static value, or calls the reactive closure. When called
    /// inside an effect, a reactive prop's read registers the effect's subscription.
    pub fn get(&self) -> T {
        match self {
            Prop::Static(value) => value.clone(),
            Prop::Reactive(f) => f(),
        }
    }
}

#[cfg(test)]
mod tests {
    //! These reactive tests use the synchronous `ImmediateEffect`, so they are hang-free under the
    //! default multi-threaded test harness — including on Windows — and need no `#[ignore]` or
    //! `--test-threads=1`. (A multi-threaded harness can deadlock only with *async*,
    //! executor-driven effects, which this crate does not use.) Each test keeps its `Owner` alive
    //! for its whole body, since `Owner::set` stores only a `Weak`.

    use super::*;
    use reactive_graph::owner::Owner;
    use reactive_graph::prelude::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    #[derive(Clone, PartialEq, Debug)]
    struct Theme(u32);

    #[test]
    fn signal_write_should_fire_subscribed_effect() {
        // Keep `owner` alive for the whole test: it owns the scoped effect below, and `set`
        // stores only a Weak (RK-003). Synchronous `ImmediateEffect` is hang-free (RK-005).
        let owner = Owner::new();
        owner.set();

        let (read, write) = create_signal(0_u32);
        let fired = Arc::new(AtomicBool::new(false));

        create_effect({
            let fired = Arc::clone(&fired);
            move || {
                let _ = read.get();
                fired.store(true, Ordering::SeqCst);
            }
        });

        // Runs once on creation; clear the flag to observe the write independently.
        assert!(fired.load(Ordering::SeqCst), "effect runs once on creation");
        fired.store(false, Ordering::SeqCst);

        write.set(1);
        assert!(
            fired.load(Ordering::SeqCst),
            "a signal write should synchronously fire the subscribed effect"
        );
    }

    #[test]
    fn use_context_should_resolve_provided_value() {
        let owner = Owner::new();
        owner.set();

        provide_context(Theme(7));

        assert_eq!(use_context::<Theme>(), Some(Theme(7)));
    }

    #[test]
    fn use_context_should_return_none_when_absent() {
        let owner = Owner::new();
        owner.set();

        assert_eq!(use_context::<Theme>(), None);
    }

    #[test]
    fn prop_from_value_should_be_static() {
        let p: Prop<u32> = 5.into();
        assert!(matches!(p, Prop::Static(5)));
        assert_eq!(p.get(), 5);
    }

    #[test]
    fn prop_rx_should_evaluate_closure() {
        let p = rx(|| 9_u32);
        assert!(matches!(p, Prop::Reactive(_)));
        assert_eq!(p.get(), 9);
    }

    #[test]
    fn create_memo_should_recompute_only_when_dependency_changes() {
        let owner = Owner::new();
        owner.set();

        let (read, write) = create_signal(2_u32);
        let runs = Arc::new(AtomicUsize::new(0));
        let memo = create_memo({
            let runs = Arc::clone(&runs);
            move || {
                runs.fetch_add(1, Ordering::SeqCst);
                read.get() * 10
            }
        });

        // Pull-based: the first read computes it (adapting away reactive_graph's prev value).
        assert_eq!(memo.get(), 20);
        let after_first = runs.load(Ordering::SeqCst);

        // A dependency change recomputes on the next read.
        write.set(3);
        assert_eq!(memo.get(), 30);
        assert!(
            runs.load(Ordering::SeqCst) > after_first,
            "a dependency change should recompute the memo"
        );
    }
}
