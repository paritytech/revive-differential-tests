use crate::internal_prelude::*;

#[derive(Clone, Debug)]
pub struct Pool<T> {
    items: Arc<[T]>,
    index: Arc<AtomicUsize>,
    len: usize,
}

impl<T> Pool<T> {
    #[allow(dead_code)]
    pub fn new(items: impl IntoIterator<Item = T>) -> anyhow::Result<Self> {
        let items = items.into_iter().collect::<Arc<[T]>>();
        if items.is_empty() {
            anyhow::bail!("No items were provided to the pool")
        }
        Ok(Self {
            len: items.len(),
            items,
            index: Default::default(),
        })
    }

    pub(crate) fn new_unchecked(items: impl IntoIterator<Item = T>) -> Self {
        let items = items.into_iter().collect::<Arc<[T]>>();
        Self {
            len: items.len(),
            items,
            index: Default::default(),
        }
    }

    pub fn next(&self) -> &T {
        let index = self
            .index
            .fetch_add(1, std::sync::atomic::Ordering::Release)
            % self.len;
        &self.items[index]
    }
}

impl<T> Deref for Pool<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.next()
    }
}

impl<T> AsRef<T> for Pool<T> {
    fn as_ref(&self) -> &T {
        self.next()
    }
}

#[derive(Clone, Debug)]
pub enum SingleOrPool<T> {
    Single(T),
    Pool(Pool<T>),
}

impl<T> SingleOrPool<T> {
    pub fn item(&self) -> &T {
        match self {
            Self::Single(item) => item,
            Self::Pool(pool) => pool.next(),
        }
    }
}

impl<T> Deref for SingleOrPool<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.item()
    }
}

impl<T> AsRef<T> for SingleOrPool<T> {
    fn as_ref(&self) -> &T {
        self.item()
    }
}
