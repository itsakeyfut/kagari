//! Events & input (Phase 5, specs §1.5/§1.9): hit-test, dispatch, focus, actions, gestures, DnD,
//! cursor. So far: the [`HitTest`] structure (#47) — the data-oriented "what is under the pointer?"
//! query the rest of the input system builds on.

mod hit_test;

pub use hit_test::{HitRegion, HitTest, InteractFlags};
