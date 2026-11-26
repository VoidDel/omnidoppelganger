//! Poison-tolerant extension for [`std::sync::Mutex`].

use std::sync::{Mutex, MutexGuard};

/// Extension on [`Mutex`] that recovers the inner data if another thread panicked while holding
/// the lock, preventing cascading panics across unrelated sessions or background tasks.
pub trait MutexExt<T> {
    fn lock_unpoisoned(&self) -> MutexGuard<'_, T>;
}

impl<T> MutexExt<T> for Mutex<T> {
    #[inline]
    fn lock_unpoisoned(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::MutexExt;

    #[test]
    fn recovers_inner_value_after_poisoning_without_panicking() {
        let shared = Arc::new(Mutex::new(vec![1, 2]));
        let poisoned_clone = shared.clone();
        let join = std::thread::spawn(move || {
            let mut guard = poisoned_clone.lock().unwrap();
            guard.push(3);
            panic!("simulated panic while holding mutex");
        });
        assert!(join.join().is_err());
        assert!(shared.lock().is_err());

        let mut recovered = shared.lock_unpoisoned();
        assert_eq!(*recovered, vec![1, 2, 3]);
        recovered.push(4);
        drop(recovered);
        assert_eq!(*shared.lock_unpoisoned(), vec![1, 2, 3, 4]);
    }
}
