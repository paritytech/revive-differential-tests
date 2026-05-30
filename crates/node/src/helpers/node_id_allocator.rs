use crate::internal_prelude::*;

static IDS: LazyLock<Arc<StdMutex<BTreeMap<String, AtomicU32>>>> = LazyLock::new(Default::default);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u32);

impl NodeId {
    pub fn for_node(node: impl ToString) -> Self {
        let mut ids = IDS.lock().expect("poisoned");
        Self(
            ids.entry(node.to_string())
                .or_default()
                .fetch_add(1, Ordering::Relaxed),
        )
    }
}
