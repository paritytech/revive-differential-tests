/// This constant defines how much Wei accounts are pre-seeded with in genesis.
///
/// We use [`u128::MAX`] here which means that accounts will be given 2^128 - 1 WEI which is
/// (2^128 - 1) / 10^18 ETH.
pub const INITIAL_BALANCE: u128 = u128::MAX;
