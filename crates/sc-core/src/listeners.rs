use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

/// The guarded values stay consistent even if a holder panicked, so poisoning is ignored.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Registry<F: ?Sized> {
    next_id: u64,
    entries: Vec<(u64, Arc<F>)>,
}

/// A set of callbacks. Callers take a [`snapshot`](Listeners::snapshot) and call it outside any
/// lock, so a listener may read back into whatever notified it.
pub(crate) struct Listeners<F: ?Sized> {
    registry: Arc<Mutex<Registry<F>>>,
}

impl<F: ?Sized + Send + Sync + 'static> Listeners<F> {
    pub fn new() -> Self {
        Self {
            registry: Arc::new(Mutex::new(Registry {
                next_id: 0,
                entries: Vec::new(),
            })),
        }
    }

    pub fn add(&self, listener: Arc<F>) -> ListenerGuard {
        let id = {
            let mut registry = lock(&self.registry);
            registry.next_id += 1;
            let id = registry.next_id;
            registry.entries.push((id, listener));
            id
        };
        let registry: Weak<Mutex<Registry<F>>> = Arc::downgrade(&self.registry);
        ListenerGuard {
            remove: Some(Box::new(move || {
                if let Some(registry) = registry.upgrade() {
                    lock(&registry).entries.retain(|(i, _)| *i != id);
                }
            })),
        }
    }

    pub fn snapshot(&self) -> Vec<Arc<F>> {
        lock(&self.registry)
            .entries
            .iter()
            .map(|(_, l)| Arc::clone(l))
            .collect()
    }
}

/// A registered listener; dropping it unsubscribes.
#[must_use = "dropping a ListenerGuard unsubscribes it"]
pub struct ListenerGuard {
    remove: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl ListenerGuard {
    pub fn unsubscribe(self) {}
}

impl Drop for ListenerGuard {
    fn drop(&mut self) {
        if let Some(remove) = self.remove.take() {
            remove();
        }
    }
}

impl fmt::Debug for ListenerGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ListenerGuard")
    }
}
