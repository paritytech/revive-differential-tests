/// This constant defines how much Wei accounts are pre-seeded with in genesis.
///
/// Note: After changing this number, check that the tests for substrate work as we encountered
/// some issues with different values of the initial balance on substrate.
pub const INITIAL_BALANCE: u128 = 10u128.pow(37);
