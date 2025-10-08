use std::sync::atomic::{AtomicUsize, Ordering};

pub struct RoundRobinPool<T> {
	next_index: AtomicUsize,
	items: Vec<T>,
}

impl<T> RoundRobinPool<T> {
	pub fn new(items: Vec<T>) -> Self {
		Self { next_index: Default::default(), items }
	}

	pub fn round_robin(&self) -> &T {
		let current = self.next_index.fetch_add(1, Ordering::SeqCst) % self.items.len();
		self.items.get(current).unwrap()
	}

	pub fn iter(&self) -> impl Iterator<Item = &T> {
		self.items.iter()
	}
}
