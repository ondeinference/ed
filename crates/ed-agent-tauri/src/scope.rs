//! Which session and message a running turn belongs to.

use std::sync::{Arc, Mutex};

/// Shared between the sink and the approval handler so that events emitted
/// from inside Ed (which knows nothing about sessions) can still say which
/// session they belong to.
///
/// Ed runs one turn at a time, so one slot is enough.
#[derive(Clone, Default)]
pub(crate) struct TurnScope(Arc<Mutex<Option<(String, String)>>>);

impl TurnScope {
    pub(crate) fn set(&self, session: &str, id: &str) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((session.to_owned(), id.to_owned()));
    }

    pub(crate) fn clear(&self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// `(session, id)`, both empty outside a `submit`.
    pub(crate) fn get(&self) -> (String, String) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_default()
    }
}
