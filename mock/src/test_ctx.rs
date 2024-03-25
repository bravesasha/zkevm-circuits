//! Mock types and functions to generate Test enviroments for ZKEVM tests

use crate::{eth, MockAccount, MockBlock, MockTransaction, MOCK_WALLETS};
#[cfg(feature = "scroll")]
use eth_types::l2_types::BlockTrace;
use eth_types::{
    geth_types::{Account, BlockConstants, GethData},
    BigEndianHash, Block, Bytecode, Error, Transaction, Word, H256,
};
#[cfg(feature = "scroll")]
use external_tracer::l2trace;
#[cfg(not(feature = "scroll"))]
use external_tracer::trace;
use external_tracer::TraceConfig;
use helpers::*;
use itertools::Itertools;

pub use external_tracer::LoggerConfig;

/// TestContext is a type that contains all the information from a block
/// required to build the circuit inputs.
///
/// It is specifically used to generate Test cases with very precise information
/// details about any specific part of a block. That includes of course, its
/// transactions too and the accounts involved in all of them.
///
/// The intended way to interact with the structure is through the fn `new`
/// which is designed to return a [`GethData`] which then can be used to query
/// any specific part of the logs generated by the transactions executed within
/// this context.
///
/// ## Example
/// ```rust
/// use eth_types::evm_types::{stack::Stack, Gas, OpcodeId};
/// use eth_types::{address, bytecode, geth_types::GethData, word, Bytecode, ToWord, Word};
/// use mock::test_ctx::{helpers::*, TestContext};
/// // code_a calls code
/// // jump to 0x10 which is outside the code (and also not marked with
///         // JUMPDEST)
/// let code = bytecode! {
///     PUSH1(0x10)
///     JUMP
///     STOP
/// };
/// let code_a = bytecode! {
///     PUSH1(0x0) // retLength
///     PUSH1(0x0) // retOffset
///     PUSH1(0x0) // argsLength
///     PUSH1(0x0) // argsOffset
///     PUSH32(address!("0x000000000000000000000000000000000cafe001").to_word()) // addr
///     PUSH32(0x1_0000) // gas
///     STATICCALL
///     PUSH2(0xaa)
/// };
/// let index = 8; // JUMP
///
/// // Get the execution steps from the external tracer
/// let block: GethData = TestContext::<3, 2>::new(
///     None,
///     |accs| {
///         accs[0]
///             .address(address!("0x0000000000000000000000000000000000000000"))
///             .code(code_a);
///         accs[1].address(address!("0x000000000000000000000000000000000cafe001")).code(code);
///         accs[2]
///             .address(address!("0x000000000000000000000000000000000cafe002"))
///             .balance(Word::from(1u64 << 30));
///     },
///     |mut txs, accs| {
///         txs[0].to(accs[0].address).from(accs[2].address);
///         txs[1]
///             .to(accs[1].address)
///             .from(accs[2].address)
///             .nonce(Word::one());
///     },
///     |block, _tx| block.number(0xcafeu64),
/// )
/// .unwrap()
/// .into();
///
/// // Now we can start generating the traces and items we need to inspect
/// // the behaviour of the generated env.
/// ```
#[derive(Debug)]
pub struct TestContext<const NACC: usize, const NTX: usize> {
    /// chain id
    pub chain_id: u64,
    /// Account list
    pub accounts: [Account; NACC],
    /// history hashes contains most recent 256 block hashes in history, where
    /// the lastest one is at history_hashes[history_hashes.len() - 1].
    pub history_hashes: Vec<Word>,
    /// Block from geth
    pub eth_block: eth_types::Block<eth_types::Transaction>,
    /// Execution Trace from geth
    pub geth_traces: Vec<eth_types::GethExecTrace>,
    #[cfg(feature = "scroll")]
    block_trace: BlockTrace,
}

impl<const NACC: usize, const NTX: usize> From<TestContext<NACC, NTX>> for GethData {
    fn from(ctx: TestContext<NACC, NTX>) -> GethData {
        GethData {
            chain_id: ctx.chain_id,
            history_hashes: ctx.history_hashes,
            eth_block: ctx.eth_block,
            geth_traces: ctx.geth_traces.to_vec(),
            accounts: ctx.accounts.into(),
            #[cfg(feature = "scroll")]
            block_trace: ctx.block_trace,
        }
    }
}

impl<const NACC: usize, const NTX: usize> TestContext<NACC, NTX> {
    pub fn new_with_logger_config<FAcc, FTx, Fb>(
        history_hashes: Option<Vec<Word>>,
        acc_fns: FAcc,
        func_tx: FTx,
        func_block: Fb,
        logger_config: LoggerConfig,
    ) -> Result<Self, Error>
    where
        FTx: FnOnce(Vec<&mut MockTransaction>, [MockAccount; NACC]),
        Fb: FnOnce(&mut MockBlock, Vec<MockTransaction>) -> &mut MockBlock,
        FAcc: FnOnce([&mut MockAccount; NACC]),
    {
        let mut accounts: Vec<MockAccount> = vec![MockAccount::default(); NACC];
        // Build Accounts modifiers
        let account_refs = accounts
            .iter_mut()
            .collect_vec()
            .try_into()
            .expect("Mismatched len err");
        acc_fns(account_refs);
        let accounts: [MockAccount; NACC] = accounts
            .iter_mut()
            .map(|acc| acc.build())
            .collect_vec()
            .try_into()
            .expect("Mismatched acc len");

        let mut transactions = vec![MockTransaction::default(); NTX];
        // By default, set the TxIndex and the Nonce values of the multiple transactions
        // of the context correlative so that any Ok test passes by default.
        // If the user decides to override these values, they'll then be set to whatever
        // inputs were provided by the user.
        transactions
            .iter_mut()
            .enumerate()
            .skip(1)
            .for_each(|(idx, tx)| {
                tx.transaction_idx(u64::try_from(idx).expect("Unexpected idx conversion error"));
                tx.nonce(Word::from(
                    u64::try_from(idx).expect("Unexpected idx conversion error"),
                ));
            });
        let tx_refs = transactions.iter_mut().collect();

        // Build Tx modifiers.
        func_tx(tx_refs, accounts.clone());
        let transactions: Vec<MockTransaction> =
            transactions.iter_mut().map(|tx| tx.build()).collect();

        // Build Block modifiers
        let mut block = MockBlock::default();
        let parent_hash = history_hashes
            .as_ref()
            .and_then(|hashes| hashes.last().copied())
            .unwrap_or_default();
        block.parent_hash(H256::from_uint(&parent_hash));
        log::debug!("block.transactions len {}", block.transactions.len());

        block.transactions.extend_from_slice(&transactions);
        func_block(&mut block, transactions).build();

        let chain_id = block.chain_id;
        let block = Block::<Transaction>::from(block);
        let accounts: [Account; NACC] = accounts
            .iter()
            .cloned()
            .map(Account::from)
            .collect_vec()
            .try_into()
            .expect("Mismatched acc len");

        let trace_config = gen_trace_config(
            chain_id,
            block.clone(),
            accounts.to_vec(),
            history_hashes.clone(),
            logger_config,
        )?;

        #[cfg(feature = "scroll")]
        let block_trace = l2trace(&trace_config)?;

        #[cfg(feature = "scroll")]
        let geth_traces = block_trace
            .execution_results
            .clone()
            .into_iter()
            .map(From::from)
            .collect::<Vec<_>>();

        #[cfg(not(feature = "scroll"))]
        let geth_traces = trace(&trace_config)?;

        Ok(Self {
            chain_id,
            accounts,
            history_hashes: history_hashes.unwrap_or_default(),
            eth_block: block,
            geth_traces,
            #[cfg(feature = "scroll")]
            block_trace,
        })
    }

    /// Create a new TestContext which starts with `NACC` default accounts and
    /// `NTX` default transactions.  Afterwards, we apply the `acc_fns`
    /// function to the accounts, the `func_tx` to the transactions and
    /// the `func_block` to the block, where each of these functions can
    /// mutate their target using the builder pattern. Finally an
    /// execution trace is generated of the resulting input block and state.
    pub fn new<FAcc, FTx, Fb>(
        history_hashes: Option<Vec<Word>>,
        acc_fns: FAcc,
        func_tx: FTx,
        func_block: Fb,
    ) -> Result<Self, Error>
    where
        FTx: FnOnce(Vec<&mut MockTransaction>, [MockAccount; NACC]),
        Fb: FnOnce(&mut MockBlock, Vec<MockTransaction>) -> &mut MockBlock,
        FAcc: FnOnce([&mut MockAccount; NACC]),
    {
        Self::new_with_logger_config(
            history_hashes,
            acc_fns,
            func_tx,
            func_block,
            LoggerConfig::default(),
        )
    }

    /// obtain the full l2 block trace
    #[cfg(feature = "scroll")]
    pub fn l2_trace(&self) -> &BlockTrace {
        &self.block_trace
    }

    /// Returns a simple TestContext setup with a single tx executing the
    /// bytecode passed as parameters. The balances of the 2 accounts and
    /// addresses are the ones used in [`TestContext::
    /// account_0_code_wallet_0_no_code`]. Extra accounts, txs and/or block
    /// configs are set as [`Default`].
    pub fn simple_ctx_with_bytecode(bytecode: Bytecode) -> Result<TestContext<2, 1>, Error> {
        TestContext::new(
            None,
            account_0_code_wallet_0_no_code(bytecode),
            |mut txs, accs| {
                txs[0].from(MOCK_WALLETS[0].clone()).to(accs[0].address);
            },
            |block, _txs| block.number(0xcafeu64),
        )
    }
}

/// Generates config to generating execution traces for the transactions included in the provided
/// Block
pub fn gen_trace_config(
    chain_id: u64,
    block: Block<Transaction>,
    accounts: Vec<Account>,
    history_hashes: Option<Vec<Word>>,
    logger_config: LoggerConfig,
) -> Result<TraceConfig, Error> {
    Ok(TraceConfig {
        chain_id,
        history_hashes: history_hashes.unwrap_or_default(),
        block_constants: BlockConstants::try_from(&block)?,
        accounts: accounts
            .iter()
            .map(|account| (account.address, account.clone()))
            .collect(),
        transactions: block
            .transactions
            .iter()
            .map(eth_types::geth_types::Transaction::from)
            .collect(),
        logger_config,
        #[cfg(feature = "shanghai")]
        chain_config: Some(external_tracer::ChainConfig::shanghai()),
        #[cfg(not(feature = "shanghai"))]
        chain_config: None,
        #[cfg(feature = "scroll")]
        l1_queue_index: 0,
    })
}

/// Collection of helper functions which contribute to specific rutines on the
/// builder pattern used to construct [`TestContext`]s.
pub mod helpers {
    use super::*;
    use crate::{MOCK_ACCOUNTS, MOCK_WALLETS};
    use ethers_signers::Signer;

    /// Generate a simple setup which adds balance to two default accounts from
    /// [`static@MOCK_ACCOUNTS`]:
    /// - 0x000000000000000000000000000000000cafe111
    /// - 0x000000000000000000000000000000000cafe222
    /// And injects the provided bytecode into the first one.
    pub fn account_0_code_account_1_no_code(code: Bytecode) -> impl FnOnce([&mut MockAccount; 2]) {
        |accs| {
            accs[0]
                .address(MOCK_ACCOUNTS[0])
                .balance(eth(10))
                .code(code);
            accs[1].address(MOCK_ACCOUNTS[1]).balance(eth(10));
        }
    }

    /// Generate a setup which adds balance to two default accounts,
    /// the receiver is from
    /// [`static@MOCK_ACCOUNTS`]:
    /// - 0x000000000000000000000000000000000cafe111
    /// and sender is a random wallet account from the first of
    /// [`static@MOCK_WALLETS`];
    /// And injects the provided bytecode into the first one.
    pub fn account_0_code_wallet_0_no_code(code: Bytecode) -> impl FnOnce([&mut MockAccount; 2]) {
        |accs| {
            accs[0]
                .address(MOCK_ACCOUNTS[0])
                .balance(eth(10))
                .code(code);
            accs[1].address(MOCK_WALLETS[0].address()).balance(eth(10));
        }
    }

    /// Generate a single transaction from the second account of the list to the
    /// first one.
    pub fn tx_from_1_to_0(mut txs: Vec<&mut MockTransaction>, accs: [MockAccount; 2]) {
        txs[0].from(accs[1].address).to(accs[0].address);
    }
}
