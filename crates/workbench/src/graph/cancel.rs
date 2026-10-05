//! `AlgoCancel` — the ONE cancellation primitive every algorithm loop (GRA-01/GRA-06's
//! cancellation requirement) and every session-registry entry (plan 44-07) checks/wraps
//! (plan 44-15 Task 1).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A cheaply-cloneable, shared cancellation flag. Every clone observes the same underlying
/// `AtomicBool` — cancelling any handle cancels every clone of it, which is exactly the shape
/// plan 44-07's session registry needs: hand one clone to the running algorithm task and keep
/// another in the registry entry to cancel it from a later request.
#[derive(Clone, Debug)]
pub struct AlgoCancel(Arc<AtomicBool>);

impl AlgoCancel {
    /// A fresh, not-yet-cancelled handle.
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    /// `true` once [`Self::cancel`] has been called on this handle or any of its clones.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// Mark this handle (and every clone sharing its underlying state) as cancelled.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// `true` iff `self` and `other` share the same underlying cancellation flag (pointer
    /// identity of the inner `Arc`, not merely equal cancelled-state). Plan 44-07's
    /// `SessionRegistry::finish` needs this to detect a late-finishing job that a newer `Run`
    /// request has already superseded with a DIFFERENT `AlgoCancel` — comparing cancelled-state
    /// alone cannot distinguish "the current job, not yet cancelled" from "a stale job that
    /// happens to also not be cancelled".
    pub fn same_as(&self, other: &AlgoCancel) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Default for AlgoCancel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_is_not_cancelled() {
        let c = AlgoCancel::new();
        assert!(!c.is_cancelled());
    }

    #[test]
    fn cancel_sets_is_cancelled_true() {
        let c = AlgoCancel::new();
        c.cancel();
        assert!(c.is_cancelled());
    }

    #[test]
    fn a_clone_observes_the_same_cancellation_shared_state() {
        let original = AlgoCancel::new();
        let clone = original.clone();
        assert!(!clone.is_cancelled());

        original.cancel();

        assert!(
            clone.is_cancelled(),
            "cancelling the original must be observed by every clone (shared underlying state)"
        );
    }

    #[test]
    fn same_as_is_true_for_clones_and_false_for_independent_handles() {
        let a = AlgoCancel::new();
        let a_clone = a.clone();
        let b = AlgoCancel::new();

        assert!(
            a.same_as(&a_clone),
            "a clone shares the same underlying Arc"
        );
        assert!(
            !a.same_as(&b),
            "two independently constructed handles must not be same_as"
        );
    }
}
