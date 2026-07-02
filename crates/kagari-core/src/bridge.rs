//! Background → UI bridge (#66, specs §7.2/§7.4): a runtime-agnostic way for background work (the
//! consumer's tokio/scheduler) to update the UI, one-way. A [`UiProxy`] posts a **UI closure** that
//! the app runs on the UI thread (under the root reactive owner) to write signals — never touching UI
//! state cross-thread. `kagari-core` adds no async runtime; the consumer's runtime drives the work.

use std::sync::{Arc, OnceLock};

use winit::event_loop::EventLoopProxy;

use crate::reactive::Owner;

/// The winit user-event payload: a boxed closure run on the UI thread. Internal — the public surface
/// is [`UiProxy`].
pub(crate) type UiEvent = Box<dyn FnOnce() + Send + 'static>;

/// The winit event loop's user-event type: either a background-posted UI closure (#66) or an
/// accessibility event forwarded by the `accesskit_winit` adapter (#67). One enum because a winit
/// event loop has a single user-event type; the app's `user_event` matches on it.
pub(crate) enum UserEvent {
    /// A background-posted UI closure to run on the UI thread (#66).
    Ui(UiEvent),
    /// An accessibility event from the `accesskit_winit` adapter (#67): initial-tree request, an
    /// action request, or deactivation.
    Accessibility(accesskit_winit::Event),
}

impl From<accesskit_winit::Event> for UserEvent {
    fn from(event: accesskit_winit::Event) -> Self {
        UserEvent::Accessibility(event)
    }
}

/// A cloneable, `Send` handle a background task uses to post work to the UI thread (#66). Obtain one
/// from [`App::ui_proxy`](crate::App::ui_proxy) and move a clone into any task; [`spawn`](Self::spawn)
/// runs a closure on the UI thread. One-way: the closure updates signals — it must not read or mutate
/// UI state directly (that lives on the UI thread).
///
/// The underlying `EventLoopProxy` only exists once [`App::run`](crate::App::run) creates the event
/// loop, so this wraps a deferred cell: `ui_proxy()` may be called before `run`, and a closure posted
/// before the loop starts is dropped (with a warning) rather than delivered.
#[derive(Clone)]
pub struct UiProxy {
    cell: Arc<OnceLock<EventLoopProxy<UserEvent>>>,
}

impl UiProxy {
    /// Wraps the app's deferred proxy cell (populated by [`App::run`](crate::App::run)).
    pub(crate) fn new(cell: Arc<OnceLock<EventLoopProxy<UserEvent>>>) -> Self {
        Self { cell }
    }

    /// Posts `f` to run on the UI thread, waking the event loop. Use it from a background task to
    /// **update existing signals**, e.g. `proxy.spawn(move || my_signal.set(value))`. The closure runs
    /// under the app root owner, so it should update signals rather than create new reactive scopes
    /// (`create_effect` / new signals) — those would live for the app's whole lifetime. Dropped (with
    /// a warning) if the event loop has not started yet, or dropped silently once it has exited.
    pub fn spawn(&self, f: impl FnOnce() + Send + 'static) {
        match self.cell.get() {
            // `send_event` errors only if the loop has closed (app exiting) — then the work is moot.
            Some(proxy) => {
                let _ = proxy.send_event(UserEvent::Ui(Box::new(f)));
            }
            None => {
                tracing::warn!("UiProxy::spawn called before the event loop started; work dropped")
            }
        }
    }
}

/// Runs a posted UI closure on the UI thread under `owner` (the app root owner), so any signal write
/// it makes is tracked and its synchronous effects fire → damage → redraw next frame (#66). Called by
/// the app's `ApplicationHandler::user_event`.
pub(crate) fn run_ui_event(owner: &Owner, event: UiEvent) {
    owner.with(event);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reactive::RwSignal;
    use crate::reactive::prelude::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn ui_event_closure_should_update_signal_under_owner() {
        // The delivery-side contract: a posted UI closure, run under an owner, updates a signal.
        // Synchronous ImmediateEffect; hold the owner to the end (RK-003/005/008).
        let owner = Owner::new();
        owner.set();
        let sig = RwSignal::new(0u32);
        let event: UiEvent = Box::new(move || sig.set(9));
        run_ui_event(&owner, event);
        assert_eq!(
            sig.get_untracked(),
            9,
            "the posted closure runs on the UI thread and updates the signal"
        );
        drop(owner);
    }

    #[test]
    fn ui_proxy_spawn_before_loop_should_drop_closure() {
        // Before `run` populates the cell, spawn must not run the closure (nor panic).
        let proxy = UiProxy::new(Arc::new(OnceLock::new()));
        let ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ran);
        proxy.spawn(move || flag.store(true, Ordering::SeqCst));
        assert!(
            !ran.load(Ordering::SeqCst),
            "a pre-loop spawn drops the closure (no delivery, no panic)"
        );
    }
}
