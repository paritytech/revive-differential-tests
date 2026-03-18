use crate::internal_prelude::*;

#[derive(Clone, Debug, Default)]
pub struct AsyncHashMap<K, V> {
    inner: Arc<Mutex<HashMap<K, Slot<V>>>>,
}

#[derive(Clone, Debug)]
enum Slot<V> {
    /// An empty slot which doesn't contain a value but contains a notification sender which would
    /// notify other tasks when the field has a value
    Empty(Arc<Notify>),
    /// A field which contains value.
    Value(V),
}

impl<K, V> AsyncHashMap<K, V>
where
    K: Eq + Hash + Send + Clone + 'static,
    V: Clone + Send + 'static,
{
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn insert(&self, key: K, value: V) -> FrameworkFuture<()> {
        let inner_map = self.inner.clone();
        Box::pin(async move {
            // Locking the outer map.
            let mut mutex_guard = inner_map.lock().await;

            // Getting the inner value of the slot.
            let entry = mutex_guard
                .entry(key)
                .or_insert_with(|| Slot::Empty(Arc::new(Notify::new())));

            let notify = if let Slot::Empty(notify) = entry {
                Some(notify.clone())
            } else {
                None
            };

            // Update the entry's value
            *entry = Slot::Value(value);

            // Notify all of the watcher's for this field.
            if let Some(notify) = notify {
                notify.notify_waiters();
            }
        })
    }

    pub fn get(&self, k: K) -> FrameworkFuture<V> {
        let inner_map = self.inner.clone();
        Box::pin(async move {
            // Locking the outer map.
            let mut mutex_guard = inner_map.lock().await;

            // Getting the inner value of the slot.
            let entry = mutex_guard
                .entry(k.clone())
                .or_insert_with(|| Slot::Empty(Arc::new(Notify::new())));

            // Check the value of the slot.
            match entry {
                // There's a notify in the slot. Therefore. We want to drop all of the guards we
                // have and then wait until we receive a notification.
                Slot::Empty(notify) => {
                    let notify = notify.clone();
                    let notified = notify.notified();

                    drop(mutex_guard);

                    // Wait until we get a notification of the value being set.
                    notified.await;

                    // We've just received a notification. So, we go ahead and obtain the value of
                    // the field.
                    let Some(Slot::Value(value)) = inner_map.lock().await.get(&k).cloned() else {
                        unreachable!()
                    };
                    value
                }
                // There already exists a value in this slot. Return it immediately
                Slot::Value(value) => value.clone(),
            }
        })
    }
}
