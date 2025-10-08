use alloy::{primitives::U256, signers::local::PrivateKeySigner};
use anyhow::{Context, Result, bail};

/// This is a sequential private key allocator. When instantiated, it allocated private keys in
/// sequentially and in order until the maximum private key specified is reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrivateKeyAllocator {
	/// The next private key to be returned by the allocator when requested.
	next_private_key: U256,

	/// The highest private key (exclusive) that can be returned by this allocator.
	highest_private_key_inclusive: U256,
}

impl PrivateKeyAllocator {
	/// Creates a new instance of the private key allocator.
	pub fn new(highest_private_key_inclusive: U256) -> Self {
		Self { next_private_key: U256::ONE, highest_private_key_inclusive }
	}

	/// Allocates a new private key and errors out if the maximum private key has been reached.
	pub fn allocate(&mut self) -> Result<PrivateKeySigner> {
		if self.next_private_key > self.highest_private_key_inclusive {
			bail!("Attempted to allocate a private key but failed since all have been allocated");
		};
		let private_key =
			PrivateKeySigner::from_slice(self.next_private_key.to_be_bytes::<32>().as_slice())
				.context("Failed to convert the private key digits into a private key")?;
		self.next_private_key += U256::ONE;
		Ok(private_key)
	}
}
