//! Integration spike for the reactive runtime adopted in #28 (`reactive_graph`).
//!
//! Proves the signal -> effect -> node-dirty integration shape that the element
//! tree (#29+) is built on: a signal write synchronously re-runs the subscribed
//! effect, which flips a node-dirty flag. This is exactly the spec's §1.4 flow
//! ("signal write -> mark element layout/paint-dirty -> damage"), modelled here
//! with a synchronous `ImmediateEffect` so it is deterministic, plain-`#[test]`
//! testable, and free of an async executor (RK-005-safe). The downstream tasks
//! (#29 wraps this into `Prop<T>` + the node arena) only need this shape proven.
//!
//! `features = ["effects"]` must be enabled on `reactive_graph`, otherwise
//! `ImmediateEffect::new` is a no-op and the effect never runs.
//!
//! RK-005 (Windows multi-thread hang) is avoided structurally: no async executor
//! is spawned and `Owner` is thread-local, so parallel test threads share no
//! reactive state. Do NOT switch to async `Effect` here without re-checking RK-005.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use reactive_graph::{effect::ImmediateEffect, owner::Owner, prelude::*, signal::RwSignal};

#[test]
fn reactive_signal_write_should_rerun_effect_and_flip_dirty() {
    // A reactive owner must be set for arena-backed signals/effects to work.
    // Keep `owner` alive for the whole test: `set` stores only a Weak, so dropping
    // the owner would dispose the arena-owned signals/effects below.
    let owner = Owner::new();
    owner.set();

    // Stands in for a retained node's paint/layout-dirty flag (§1.4).
    let node_dirty = Arc::new(AtomicBool::new(false));
    let count = RwSignal::new(0_u32);

    // The effect subscribes to `count` and marks the node dirty whenever it runs.
    let _guard = ImmediateEffect::new({
        let node_dirty = Arc::clone(&node_dirty);
        move || {
            let _ = count.get();
            node_dirty.store(true, Ordering::SeqCst);
        }
    });

    // The effect runs once on creation, so the node starts dirty.
    assert!(
        node_dirty.load(Ordering::SeqCst),
        "effect should run once on creation"
    );

    // Clear the flag to observe the next run independently.
    node_dirty.store(false, Ordering::SeqCst);

    // Writing the subscribed signal re-runs the effect synchronously.
    count.set(1);
    assert!(
        node_dirty.load(Ordering::SeqCst),
        "signal write should synchronously re-run the subscribed effect and flip the dirty flag"
    );
}

#[test]
fn reactive_untracked_change_should_not_rerun_effect() {
    // Keep `owner` alive for the whole test (see the first test): `set` stores a Weak.
    let owner = Owner::new();
    owner.set();

    let runs = Arc::new(AtomicUsize::new(0));
    let tracked = RwSignal::new(0_u32);
    let untracked = RwSignal::new(0_u32);

    // The effect reads only `tracked`, so it subscribes only to `tracked`.
    let _guard = ImmediateEffect::new({
        let runs = Arc::clone(&runs);
        move || {
            let _ = tracked.get();
            runs.fetch_add(1, Ordering::SeqCst);
        }
    });

    assert_eq!(runs.load(Ordering::SeqCst), 1, "one run on creation");

    // Writing the tracked signal re-runs the effect.
    tracked.set(1);
    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "tracked write re-runs the effect"
    );

    // Writing an un-subscribed signal must NOT re-run the effect (fine-grained).
    untracked.set(1);
    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "untracked write must not re-run the effect (fine-grained subscription)"
    );
}
