use aggregator::ChunkInfo;
use eth_types::{l2_types::BlockTrace, H256};
use serde::{Deserialize, Serialize};
use zkevm_circuits::evm_circuit::witness::Block;

pub type WitnessBlock = Block;

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
pub struct BlockTraceJsonRpcResult {
    pub result: BlockTrace,
}
pub use eth_types::base64;

use crate::{BatchProof, ChunkProof};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChunkProvingTask {
    /// Prover can check `chunk_info` is consistent with block traces
    pub chunk_info: Option<ChunkInfo>,
    pub block_traces: Vec<BlockTrace>,
}

impl ChunkProvingTask {
    pub fn from(block_traces: Vec<BlockTrace>) -> Self {
        Self {
            block_traces,
            chunk_info: None,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.block_traces.is_empty()
    }
    /// Used for cache/load proof from disk
    pub fn identifier(&self) -> String {
        self.block_traces
            .first()
            .map_or(0, |trace: &BlockTrace| {
                trace.header.number.expect("block num").low_u64()
            })
            .to_string()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BatchProvingTask {
    pub parent_batch_hash: H256,
    pub parent_state_root: H256,
    // BatchHeaderV3 will be re-exported from zkevm-circuits/prover.
    // TODO: pub batch_header: BatchHeaderV3,
    pub chunk_proofs: Vec<ChunkProof>,
}

impl BatchProvingTask {
    /// Used for cache/load proof from disk
    pub fn identifier(&self) -> String {
        self.chunk_proofs
            .last()
            .unwrap()
            .chunk_info
            .public_input_hash()
            .to_low_u64_le()
            .to_string()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BundleProvingTask {
    chain_id: u64,
    finalized_batch_hash: H256,
    finalized_state_root: H256,
    pending_state_root: H256,
    pending_withdraw_root: H256,
    pub batch_proofs: Vec<BatchProof>,
}
