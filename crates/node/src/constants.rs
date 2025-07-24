/// This constant defines how much Wei accounts are pre-seeded with in genesis.
///
/// Note: After changing this number, check that the tests for kitchensink work as we encountered
/// some issues with different values of the initial balance on Kitchensink.
pub const INITIAL_BALANCE: u128 = 10u128.pow(37);
