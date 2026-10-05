//! Plan 44-07: one-job-per-session cancellation registry over `AlgoCancel`.
//!
//! Generalises `WorkbenchState.report_running`'s single-flight `AtomicBool` compare-exchange
//! pattern (`crate::analysis::run_handler`) from one global flag to N independent per-session
//! slots (`HashMap<String, AlgoCancel>`) — D-05/GRA-06's "one job per browser session, cancelled
//! by a new Run or by leaving the page" rule.

use std::collections::HashMap;
use std::sync::RwLock;

use super::cancel::AlgoCancel;

/// Maps a per-tab `session_id` (no auth boundary — a coordination token, per the phase 44 design notes'
/// Claude's Discretion note and this plan's threat register T-44-07-01) to the `AlgoCancel` of
/// whatever algorithm job is currently running for that session, if any.
pub struct SessionRegistry {
    active: RwLock<HashMap<String, AlgoCancel>>,
}

impl SessionRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self {
            active: RwLock::new(HashMap::new()),
        }
    }

    /// Registers a new job for `session_id`, cancelling and replacing any job already running
    /// for that session (D-05's "a new Run cancels the previous job"). Returns a clone of the
    /// fresh `AlgoCancel` for the caller to hand to the spawned algorithm task; the registry
    /// keeps its own clone (both share the same underlying `AtomicBool`) so `cancel`/`finish`
    /// can act on it later.
    pub fn start(&self, session_id: &str) -> AlgoCancel {
        let mut active = self.active.write().expect("session registry lock poisoned");
        if let Some(previous) = active.get(session_id) {
            previous.cancel();
        }
        let fresh = AlgoCancel::new();
        active.insert(session_id.to_string(), fresh.clone());
        fresh
    }

    /// Cancels the currently-registered job for `session_id`, if any. A no-op — never a panic —
    /// for an unknown or already-finished session id.
    pub fn cancel(&self, session_id: &str) {
        let active = self.active.read().expect("session registry lock poisoned");
        if let Some(current) = active.get(session_id) {
            current.cancel();
        }
    }

    /// Clears the registry entry for `session_id`, but ONLY if `token` is still the currently-
    /// registered `AlgoCancel` for that session (compared by underlying `Arc` pointer identity
    /// via `AlgoCancel::same_as`, never by cancelled-state). A late-finishing job whose token has
    /// already been superseded by a newer `start` call must not clobber the newer job's registry
    /// entry (T-44-07-02).
    pub fn finish(&self, session_id: &str, token: &AlgoCancel) {
        let mut active = self.active.write().expect("session registry lock poisoned");
        let is_current = active
            .get(session_id)
            .is_some_and(|current| current.same_as(token));
        if is_current {
            active.remove(session_id);
        }
    }
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `start` returns a fresh, not-yet-cancelled `AlgoCancel`.
    #[test]
    fn start_returns_a_fresh_uncancelled_token() {
        let registry = SessionRegistry::new();
        let token = registry.start("session-a");
        assert!(!token.is_cancelled());
    }

    /// Calling `start` again for the SAME session before the first job finished cancels the
    /// PREVIOUS token and returns a new, distinct one (D-05).
    #[test]
    fn starting_again_for_the_same_session_cancels_the_previous_token() {
        let registry = SessionRegistry::new();
        let first = registry.start("session-a");
        assert!(!first.is_cancelled());

        let second = registry.start("session-a");

        assert!(
            first.is_cancelled(),
            "starting a new job for the same session must cancel the previous job's token"
        );
        assert!(!second.is_cancelled());
        assert!(
            !first.same_as(&second),
            "the new token must be a distinct AlgoCancel, not the same one reused"
        );
    }

    /// `cancel(session_id)` cancels the currently-registered job for that session without
    /// affecting a DIFFERENT session's running job.
    #[test]
    fn cancel_only_affects_the_named_session() {
        let registry = SessionRegistry::new();
        let token_a = registry.start("session-a");
        let token_b = registry.start("session-b");

        registry.cancel("session-a");

        assert!(token_a.is_cancelled());
        assert!(
            !token_b.is_cancelled(),
            "cancelling one session must not cancel a different session's job"
        );
    }

    /// `cancel` on an unknown or already-finished session id is a no-op, never a panic.
    #[test]
    fn cancel_on_unknown_session_is_a_no_op() {
        let registry = SessionRegistry::new();
        registry.cancel("never-started");

        let token = registry.start("session-a");
        registry.finish("session-a", &token);
        // The session's entry is now gone; cancel must still be a harmless no-op.
        registry.cancel("session-a");
        assert!(!token.is_cancelled());
    }

    /// `finish(session_id, token)` only clears the registry entry if `token` is still the
    /// currently-registered one for that session — a late-finishing, already-superseded job
    /// must not clobber a newer job's registry entry.
    #[test]
    fn finish_with_a_superseded_token_does_not_clobber_the_newer_job() {
        let registry = SessionRegistry::new();
        let stale_token = registry.start("session-a");
        let current_token = registry.start("session-a"); // supersedes stale_token

        // The late-finishing stale job calls finish with its own (now-superseded) token.
        registry.finish("session-a", &stale_token);

        // The newer job's token must still be cancellable through the registry — proving its
        // registry entry survived the stale finish.
        registry.cancel("session-a");
        assert!(
            current_token.is_cancelled(),
            "the current job's registry entry must have survived the stale finish() call"
        );
    }

    /// `finish` with the CURRENT token does clear the entry — a subsequent `cancel` for that
    /// session is then a no-op.
    #[test]
    fn finish_with_the_current_token_clears_the_entry() {
        let registry = SessionRegistry::new();
        let token = registry.start("session-a");
        registry.finish("session-a", &token);

        // Entry is gone; a fresh, independent token proves cancel() found nothing to act on.
        registry.cancel("session-a");
        assert!(!token.is_cancelled());
    }
}
