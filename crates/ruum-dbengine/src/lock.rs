//! A mutex that does not poison: a panic in one test or request never blocks the next one.

use std::sync::MutexGuard;

#[derive(Debug, Default)]
pub(crate) struct Mutex<T>(std::sync::Mutex<T>);

impl<T> Mutex<T> {
    pub(crate) fn lock(&self) -> MutexGuard<'_, T> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}
