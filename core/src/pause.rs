use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Shared pause flag. Background work (indexing, from Phase 1 on) must check
/// [`PauseState::is_paused`] before doing a unit of work.
#[derive(Debug, Clone, Default)]
pub struct PauseState(Arc<AtomicBool>);

impl PauseState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_paused(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub fn set(&self, paused: bool) {
        self.0.store(paused, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_share_state() {
        let a = PauseState::new();
        let b = a.clone();
        assert!(!b.is_paused());
        a.set(true);
        assert!(b.is_paused());
    }
}
