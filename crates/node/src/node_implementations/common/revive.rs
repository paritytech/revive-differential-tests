use alloy::{
    consensus::{BlockHeader, TxEnvelope},
    network::{
        Ethereum, Network, TransactionBuilder, TransactionBuilderError, UnbuiltTransactionError,
    },
    primitives::{Address, B64, B256, BlockNumber, Bloom, Bytes, U256},
    rpc::types::eth::{Block, Header, Transaction},
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReviveNetwork;

impl Network for ReviveNetwork {
    type TxType = <Ethereum as Network>::TxType;

    type TxEnvelope = <Ethereum as Network>::TxEnvelope;

    type UnsignedTx = <Ethereum as Network>::UnsignedTx;

    type ReceiptEnvelope = <Ethereum as Network>::ReceiptEnvelope;

    type Header = ReviveHeader;

    type TransactionRequest = <Ethereum as Network>::TransactionRequest;

    type TransactionResponse = <Ethereum as Network>::TransactionResponse;

    type ReceiptResponse = <Ethereum as Network>::ReceiptResponse;

    type HeaderResponse = Header<ReviveHeader>;

    type BlockResponse = Block<Transaction<TxEnvelope>, Header<ReviveHeader>>;
}

impl TransactionBuilder<ReviveNetwork> for <Ethereum as Network>::TransactionRequest {
    fn chain_id(&self) -> Option<alloy::primitives::ChainId> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::chain_id(self)
    }

    fn set_chain_id(&mut self, chain_id: alloy::primitives::ChainId) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::set_chain_id(
            self, chain_id,
        )
    }

    fn nonce(&self) -> Option<u64> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::nonce(self)
    }

    fn set_nonce(&mut self, nonce: u64) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::set_nonce(
            self, nonce,
        )
    }

    fn take_nonce(&mut self) -> Option<u64> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::take_nonce(
            self,
        )
    }

    fn input(&self) -> Option<&alloy::primitives::Bytes> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::input(self)
    }

    fn set_input<T: Into<alloy::primitives::Bytes>>(&mut self, input: T) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::set_input(
            self, input,
        )
    }

    fn from(&self) -> Option<Address> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::from(self)
    }

    fn set_from(&mut self, from: Address) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::set_from(
            self, from,
        )
    }

    fn kind(&self) -> Option<alloy::primitives::TxKind> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::kind(self)
    }

    fn clear_kind(&mut self) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::clear_kind(
            self,
        )
    }

    fn set_kind(&mut self, kind: alloy::primitives::TxKind) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::set_kind(
            self, kind,
        )
    }

    fn value(&self) -> Option<alloy::primitives::U256> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::value(self)
    }

    fn set_value(&mut self, value: alloy::primitives::U256) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::set_value(
            self, value,
        )
    }

    fn gas_price(&self) -> Option<u128> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::gas_price(self)
    }

    fn set_gas_price(&mut self, gas_price: u128) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::set_gas_price(
            self, gas_price,
        )
    }

    fn max_fee_per_gas(&self) -> Option<u128> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::max_fee_per_gas(
            self,
        )
    }

    fn set_max_fee_per_gas(&mut self, max_fee_per_gas: u128) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::set_max_fee_per_gas(
            self, max_fee_per_gas
        )
    }

    fn max_priority_fee_per_gas(&self) -> Option<u128> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::max_priority_fee_per_gas(
            self,
        )
    }

    fn set_max_priority_fee_per_gas(&mut self, max_priority_fee_per_gas: u128) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::set_max_priority_fee_per_gas(
            self, max_priority_fee_per_gas
        )
    }

    fn gas_limit(&self) -> Option<u64> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::gas_limit(self)
    }

    fn set_gas_limit(&mut self, gas_limit: u64) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::set_gas_limit(
            self, gas_limit,
        )
    }

    fn access_list(&self) -> Option<&alloy::rpc::types::AccessList> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::access_list(
            self,
        )
    }

    fn set_access_list(&mut self, access_list: alloy::rpc::types::AccessList) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::set_access_list(
            self,
            access_list,
        )
    }

    fn complete_type(
        &self,
        ty: <ReviveNetwork as Network>::TxType,
    ) -> Result<(), Vec<&'static str>> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::complete_type(
            self, ty,
        )
    }

    fn can_submit(&self) -> bool {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::can_submit(
            self,
        )
    }

    fn can_build(&self) -> bool {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::can_build(self)
    }

    fn output_tx_type(&self) -> <ReviveNetwork as Network>::TxType {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::output_tx_type(
            self,
        )
    }

    fn output_tx_type_checked(&self) -> Option<<ReviveNetwork as Network>::TxType> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::output_tx_type_checked(
            self,
        )
    }

    fn prep_for_submission(&mut self) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::prep_for_submission(
            self,
        )
    }

    fn build_unsigned(
        self,
    ) -> alloy::network::BuildResult<<ReviveNetwork as Network>::UnsignedTx, ReviveNetwork> {
        let result = <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::build_unsigned(
            self,
        );
        match result {
            Ok(unsigned_tx) => Ok(unsigned_tx),
            Err(UnbuiltTransactionError { request, error }) => {
                Err(UnbuiltTransactionError::<ReviveNetwork> {
                    request,
                    error: match error {
                        TransactionBuilderError::InvalidTransactionRequest(tx_type, items) => {
                            TransactionBuilderError::InvalidTransactionRequest(tx_type, items)
                        }
                        TransactionBuilderError::UnsupportedSignatureType => {
                            TransactionBuilderError::UnsupportedSignatureType
                        }
                        TransactionBuilderError::Signer(error) => {
                            TransactionBuilderError::Signer(error)
                        }
                        TransactionBuilderError::Custom(error) => {
                            TransactionBuilderError::Custom(error)
                        }
                    },
                })
            }
        }
    }

    async fn build<W: alloy::network::NetworkWallet<ReviveNetwork>>(
        self,
        wallet: &W,
    ) -> Result<<ReviveNetwork as Network>::TxEnvelope, TransactionBuilderError<ReviveNetwork>>
    {
        Ok(wallet.sign_request(self).await?)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviveHeader {
    /// The Keccak 256-bit hash of the parent
    /// block’s header, in its entirety; formally Hp.
    pub parent_hash: B256,
    /// The Keccak 256-bit hash of the ommers list portion of this block; formally Ho.
    #[serde(rename = "sha3Uncles", alias = "ommersHash")]
    pub ommers_hash: B256,
    /// The 160-bit address to which all fees collected from the successful mining of this block
    /// be transferred; formally Hc.
    #[serde(rename = "miner", alias = "beneficiary")]
    pub beneficiary: Address,
    /// The Keccak 256-bit hash of the root node of the state trie, after all transactions are
    /// executed and finalisations applied; formally Hr.
    pub state_root: B256,
    /// The Keccak 256-bit hash of the root node of the trie structure populated with each
    /// transaction in the transactions list portion of the block; formally Ht.
    pub transactions_root: B256,
    /// The Keccak 256-bit hash of the root node of the trie structure populated with the receipts
    /// of each transaction in the transactions list portion of the block; formally He.
    pub receipts_root: B256,
    /// The Bloom filter composed from indexable information (logger address and log topics)
    /// contained in each log entry from the receipt of each transaction in the transactions list;
    /// formally Hb.
    pub logs_bloom: Bloom,
    /// A scalar value corresponding to the difficulty level of this block. This can be calculated
    /// from the previous block’s difficulty level and the timestamp; formally Hd.
    pub difficulty: U256,
    /// A scalar value equal to the number of ancestor blocks. The genesis block has a number of
    /// zero; formally Hi.
    #[serde(with = "alloy::serde::quantity")]
    pub number: BlockNumber,
    /// A scalar value equal to the current limit of gas expenditure per block; formally Hl.
    // This is the main difference over the Ethereum network implementation. We use u128 here and
    // not u64.
    #[serde(with = "alloy::serde::quantity")]
    pub gas_limit: u128,
    /// A scalar value equal to the total gas used in transactions in this block; formally Hg.
    #[serde(with = "alloy::serde::quantity")]
    pub gas_used: u64,
    /// A scalar value equal to the reasonable output of Unix’s time() at this block’s inception;
    /// formally Hs.
    #[serde(with = "alloy::serde::quantity")]
    pub timestamp: u64,
    /// An arbitrary byte array containing data relevant to this block. This must be 32 bytes or
    /// fewer; formally Hx.
    pub extra_data: Bytes,
    /// A 256-bit hash which, combined with the
    /// nonce, proves that a sufficient amount of computation has been carried out on this block;
    /// formally Hm.
    pub mix_hash: B256,
    /// A 64-bit value which, combined with the mixhash, proves that a sufficient amount of
    /// computation has been carried out on this block; formally Hn.
    pub nonce: B64,
    /// A scalar representing EIP1559 base fee which can move up or down each block according
    /// to a formula which is a function of gas used in parent block and gas target
    /// (block gas limit divided by elasticity multiplier) of parent block.
    /// The algorithm results in the base fee per gas increasing when blocks are
    /// above the gas target, and decreasing when blocks are below the gas target. The base fee per
    /// gas is burned.
    #[serde(
        default,
        with = "alloy::serde::quantity::opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub base_fee_per_gas: Option<u64>,
    /// The Keccak 256-bit hash of the withdrawals list portion of this block.
    /// <https://eips.ethereum.org/EIPS/eip-4895>
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withdrawals_root: Option<B256>,
    /// The total amount of blob gas consumed by the transactions within the block, added in
    /// EIP-4844.
    #[serde(
        default,
        with = "alloy::serde::quantity::opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub blob_gas_used: Option<u64>,
    /// A running total of blob gas consumed in excess of the target, prior to the block. Blocks
    /// with above-target blob gas consumption increase this value, blocks with below-target blob
    /// gas consumption decrease it (bounded at 0). This was added in EIP-4844.
    #[serde(
        default,
        with = "alloy::serde::quantity::opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub excess_blob_gas: Option<u64>,
    /// The hash of the parent beacon block's root is included in execution blocks, as proposed by
    /// EIP-4788.
    ///
    /// This enables trust-minimized access to consensus state, supporting staking pools, bridges,
    /// and more.
    ///
    /// The beacon roots contract handles root storage, enhancing Ethereum's functionalities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_beacon_block_root: Option<B256>,
    /// The Keccak 256-bit hash of the an RLP encoded list with each
    /// [EIP-7685] request in the block body.
    ///
    /// [EIP-7685]: https://eips.ethereum.org/EIPS/eip-7685
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_hash: Option<B256>,
}

impl BlockHeader for ReviveHeader {
    fn parent_hash(&self) -> B256 {
        self.parent_hash
    }

    fn ommers_hash(&self) -> B256 {
        self.ommers_hash
    }

    fn beneficiary(&self) -> Address {
        self.beneficiary
    }

    fn state_root(&self) -> B256 {
        self.state_root
    }

    fn transactions_root(&self) -> B256 {
        self.transactions_root
    }

    fn receipts_root(&self) -> B256 {
        self.receipts_root
    }

    fn withdrawals_root(&self) -> Option<B256> {
        self.withdrawals_root
    }

    fn logs_bloom(&self) -> Bloom {
        self.logs_bloom
    }

    fn difficulty(&self) -> U256 {
        self.difficulty
    }

    fn number(&self) -> BlockNumber {
        self.number
    }

    // There's sadly nothing that we can do about this. We're required to implement this trait on
    // any type that represents a header and the gas limit type used here is a u64.
    fn gas_limit(&self) -> u64 {
        self.gas_limit.try_into().unwrap_or(u64::MAX)
    }

    fn gas_used(&self) -> u64 {
        self.gas_used
    }

    fn timestamp(&self) -> u64 {
        self.timestamp
    }

    fn mix_hash(&self) -> Option<B256> {
        Some(self.mix_hash)
    }

    fn nonce(&self) -> Option<B64> {
        Some(self.nonce)
    }

    fn base_fee_per_gas(&self) -> Option<u64> {
        self.base_fee_per_gas
    }

    fn blob_gas_used(&self) -> Option<u64> {
        self.blob_gas_used
    }

    fn excess_blob_gas(&self) -> Option<u64> {
        self.excess_blob_gas
    }

    fn parent_beacon_block_root(&self) -> Option<B256> {
        self.parent_beacon_block_root
    }

    fn requests_hash(&self) -> Option<B256> {
        self.requests_hash
    }

    fn extra_data(&self) -> &Bytes {
        &self.extra_data
    }
}
