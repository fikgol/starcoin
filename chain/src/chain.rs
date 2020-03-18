// Copyright (c) The Starcoin Core Contributors
// SPDX-License-Identifier: Apache-2.0

use actix::prelude::*;
use anyhow::{ensure, format_err, Error, Result};
use config::NodeConfig;
use consensus::Consensus;
use crypto::{hash::CryptoHash, HashValue};
use executor::mock_executor::mock_mint_txn;
use executor::TransactionExecutor;
use logger::prelude::*;
use starcoin_accumulator::{Accumulator, MerkleAccumulator};
use starcoin_statedb::ChainStateDB;
use std::convert::TryInto;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use storage::BlockChainStore;
use traits::{ChainReader, ChainState, ChainStateReader, ChainWriter, TxPoolAsyncService};
use types::{
    account_address::AccountAddress,
    block::{Block, BlockHeader, BlockInfo, BlockNumber, BlockTemplate, BLOCK_INFO_DEFAULT_ID},
    block_metadata::BlockMetadata,
    startup_info::ChainInfo,
    transaction::{SignedUserTransaction, Transaction, TransactionInfo, TransactionStatus},
    U256,
};

pub struct BlockChain<E, C, S, P>
    where
        E: TransactionExecutor,
        C: Consensus,
        S: BlockChainStore + 'static,
        P: TxPoolAsyncService + 'static,
{
    pub config: Arc<NodeConfig>,
    //TODO
    accumulator: MerkleAccumulator,
    head: Block,
    chain_state: ChainStateDB,
    phantom_e: PhantomData<E>,
    phantom_c: PhantomData<C>,
    pub storage: Arc<S>,
    pub txpool: P,
    pub chain_info: ChainInfo,
}

impl<E, C, S, P> BlockChain<E, C, S, P>
    where
        E: TransactionExecutor,
        C: Consensus,
        S: BlockChainStore,
        P: TxPoolAsyncService,
{
    pub fn new(
        config: Arc<NodeConfig>,
        chain_info: ChainInfo,
        storage: Arc<S>,
        txpool: P,
    ) -> Result<Self> {
        let head_block_hash = chain_info.get_head();
        let head = storage
            .get_block_by_hash(head_block_hash)?
            .ok_or(format_err!(
                "Can not find block by hash {}",
                head_block_hash
            ))?;
        let block_info = match storage.clone().get_block_info(head_block_hash) {
            Ok(Some(block_info_1)) => block_info_1,
            Err(e) => {
                warn!("err : {:?}", e);
                BlockInfo::new(*BLOCK_INFO_DEFAULT_ID, vec![], 0, 0)
            }
            _ => BlockInfo::new(*BLOCK_INFO_DEFAULT_ID, vec![], 0, 0),
        };

        let state_root = head.header().state_root();
        let chain = Self {
            config: config.clone(),
            accumulator: MerkleAccumulator::new(
                block_info.frozen_subtree_roots,
                block_info.num_leaves,
                block_info.num_nodes,
                storage.clone(),
            )
                .unwrap(),
            head,
            chain_state: ChainStateDB::new(storage.clone(), Some(state_root)),
            phantom_e: PhantomData,
            phantom_c: PhantomData,
            storage,
            txpool,
            chain_info,
        };
        Ok(chain)
    }

    fn verify_proof(
        &self,
        expect_root: HashValue,
        leaves: &[HashValue],
        first_leaf_idx: u64,
    ) -> Result<()> {
        ensure!(leaves.len() > 0, "invalid leaves.");
        leaves.iter().enumerate().for_each(|(i, hash)| {
            let leaf_index = first_leaf_idx + i as u64;
            let proof = self.accumulator.get_proof(leaf_index).unwrap().unwrap();
            proof.verify(expect_root, *hash, leaf_index).unwrap();
        });
        Ok(())
    }

    fn save_block(&self, block: &Block) {
        if let Err(e) = self.storage.commit_block(block.clone()) {
            warn!("err : {:?}", e);
        }
        info!("commit block : {:?}", block.header().id());
    }

    fn get_block_info(&self, block_id: HashValue) -> BlockInfo {
        let block_info = match self.storage.get_block_info(block_id) {
            Ok(Some(block_info_1)) => block_info_1,
            Err(e) => {
                warn!("err : {:?}", e);
                BlockInfo::new(*BLOCK_INFO_DEFAULT_ID, vec![], 0, 0)
            }
            _ => BlockInfo::new(*BLOCK_INFO_DEFAULT_ID, vec![], 0, 0),
        };
        block_info
    }
    fn save_block_info(&self, block_info: BlockInfo) {
        if let Err(e) = self.storage.save_block_info(block_info) {
            warn!("err : {:?}", e);
        }
    }

    fn gen_tx_for_test(&self) {
        let tx = mock_mint_txn(AccountAddress::random(), 100);
        // info!("gen test txn: {:?}", tx);
        let txpool = self.txpool.clone();
        Arbiter::spawn(async move {
            info!("gen_tx_for_test call txpool.");
            txpool.add(tx.try_into().unwrap()).await.unwrap();
        });
    }

    pub fn fork_chain_info(&self, block_id: &HashValue) -> ChainInfo {
        self.chain_info.fork(block_id).unwrap()
    }

    pub fn exist_block(&self, block_id: &HashValue) -> bool {
        self.chain_info.contains(block_id)
    }

    pub fn latest_blocks(&self) {
        self.chain_info
            .latest_blocks()
            .iter()
            .for_each(|(number, block_id)| {
                info!(
                    "block chain :: number : {} , block_id : {:?}",
                    number, block_id
                );
            });
    }

    pub fn create_block_template_inner(
        &self,
        previous_header: BlockHeader,
        difficulty: U256,
        user_txns: Vec<SignedUserTransaction>,
    ) -> Result<BlockTemplate> {
        //TODO read address from config
        let author = AccountAddress::random();
        //TODO calculate gas limit etc.
        let mut txns = user_txns
            .iter()
            .cloned()
            .map(|user_txn| Transaction::UserTransaction(user_txn))
            .collect::<Vec<Transaction>>();

        //TODO refactor BlockMetadata to Coinbase transaction.
        txns.push(Transaction::BlockMetadata(BlockMetadata::new(
            HashValue::zero(),
            0,
            author,
        )));
        let chain_state =
            ChainStateDB::new(self.storage.clone(), Some(previous_header.state_root()));
        let mut state_root = HashValue::zero();
        let mut transaction_hash = vec![];
        for txn in txns {
            let txn_hash = txn.crypto_hash();
            let output = E::execute_transaction(&self.config.vm, &chain_state, txn)?;
            match output.status() {
                TransactionStatus::Discard(status) => return Err(status.clone().into()),
                TransactionStatus::Keep(_status) => {
                    //continue.
                }
            }
            //TODO should not commit here.
            state_root = chain_state.commit()?;
            let _transaction_info = TransactionInfo::new(
                txn_hash,
                state_root,
                HashValue::zero(),
                0,
                output.status().vm_status().major_status,
            );
            transaction_hash.push(txn_hash);
        }

        let block_info = self.get_block_info(previous_header.id());

        let accumulator = MerkleAccumulator::new(
            block_info.frozen_subtree_roots,
            block_info.num_leaves,
            block_info.num_nodes,
            self.storage.clone(),
        )
            .unwrap();
        let (accumulator_root, first_leaf_idx) =
            accumulator.append_only_cache(&transaction_hash).unwrap();
        //Fixme proof verify
        transaction_hash.iter().enumerate().for_each(|(i, hash)| {
            let leaf_index = first_leaf_idx + i as u64;
            let proof = accumulator.get_proof(leaf_index).unwrap().unwrap();
            proof.verify(accumulator_root, *hash, leaf_index).unwrap();
        });
        //TODO execute txns and computer state.
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let parent_total_difficult = {
            if let Some(parent) = self.get_block_by_number(previous_header.number()).unwrap() {
                parent.header().total_difficult()
            } else {
                U256::zero()
            }
        };
        
        let total_difficult = parent_total_difficult + self.head.header().total_difficult();
        
        Ok(BlockTemplate::new(
            previous_header.id(),
            timestamp,
            previous_header.number() + 1,
            author,
            accumulator_root,
            state_root,
            0,
            0,
            difficulty,
            total_difficult,
            user_txns.into(),
        ))
    }
}

impl<E, C, S, P> ChainReader for BlockChain<E, C, S, P>
    where
        E: TransactionExecutor,
        C: Consensus,
        S: BlockChainStore,
        P: TxPoolAsyncService,
{
    fn head_block(&self) -> Block {
        self.head.clone()
    }

    fn current_header(&self) -> BlockHeader {
        self.head.header().clone()
    }

    fn get_header(&self, hash: HashValue) -> Result<Option<BlockHeader>> {
        self.storage.get_block_header_by_hash(hash)
    }

    fn get_header_by_number(&self, number: u64) -> Result<Option<BlockHeader>> {
        self.storage.get_block_header_by_number(number)
    }

    fn get_block_by_number(&self, number: BlockNumber) -> Result<Option<Block>> {
        let block_id = self.chain_info.get_hash_by_number(number);
        match block_id {
            Some(id) => self.storage.get_block_by_hash(id),
            None => Ok(None),
        }
    }

    fn get_block(&self, hash: HashValue) -> Result<Option<Block>> {
        self.storage.get_block_by_hash(hash)
    }

    fn get_transaction(&self, _hash: HashValue) -> Result<Option<Transaction>, Error> {
        unimplemented!()
    }

    fn get_transaction_info(&self, _hash: HashValue) -> Result<Option<TransactionInfo>, Error> {
        unimplemented!()
    }

    fn create_block_template(
        &self,
        parent_hash: Option<HashValue>,
        difficulty: U256,
        user_txns: Vec<SignedUserTransaction>,
    ) -> Result<BlockTemplate> {
        let previous_header = match parent_hash {
            Some(block_id) => self
                .storage
                .get_block(block_id)
                .unwrap()
                .unwrap()
                .header()
                .clone(),
            None => self.current_header(),
        };
        self.create_block_template_inner(previous_header, difficulty, user_txns)
    }

    fn chain_state_reader(&self) -> &dyn ChainStateReader {
        &self.chain_state
    }

    fn gen_tx(&self) -> Result<()> {
        self.gen_tx_for_test();
        Ok(())
    }

    fn get_chain_info(&self) -> ChainInfo {
        self.chain_info.clone()
    }

    fn get_block_info(&self) -> BlockInfo {
        self.storage
            .get_block_info(self.head.header().id())
            .unwrap()
            .unwrap()
    }

    fn get_difficulty(&self) -> U256 {
        // Caculate a difficulty for recent "block_count" blocks
        let mut block_count = 10;
        let mut current_number = self.head.header().number();
        let mut avg_target = U256::zero();
        if block_count > current_number {
            block_count = current_number
        }
        for _ in 0..block_count {
            let block = self
                .storage
                .get_block_by_number(current_number)
                .unwrap()
                .unwrap();
            avg_target = avg_target + block.header().difficult() / block_count.into();
            current_number -= 1;
        }
        avg_target
    }
}

impl<E, C, S, P> ChainWriter for BlockChain<E, C, S, P>
    where
        E: TransactionExecutor,
        C: Consensus,
        S: BlockChainStore,
        P: TxPoolAsyncService,
{
    fn apply(&mut self, block: Block) -> Result<()> {
        let header = block.header();
        info!(
            "Apply block {:?} to {:?}",
            block.header(),
            self.head.header()
        );
        //TODO custom verify macro
        assert_eq!(self.head.header().id(), block.header().parent_hash());

        C::verify_header(self.config.clone(), self, header)?;

        let chain_state = &self.chain_state;
        let mut txns = block
            .transactions()
            .iter()
            .cloned()
            .map(|user_txn| Transaction::UserTransaction(user_txn))
            .collect::<Vec<Transaction>>();
        let block_metadata = header.clone().into_metadata();

        txns.push(Transaction::BlockMetadata(block_metadata));
        let mut state_root = HashValue::zero();
        let mut transaction_hash = vec![];
        for txn in txns {
            let txn_hash = txn.crypto_hash();
            let output = E::execute_transaction(&self.config.vm, chain_state, txn)?;
            match output.status() {
                TransactionStatus::Discard(status) => return Err(status.clone().into()),
                TransactionStatus::Keep(_status) => {
                    //continue.
                }
            }
            state_root = chain_state.commit()?;
            let _transaction_info = TransactionInfo::new(
                txn_hash,
                state_root,
                HashValue::zero(),
                0,
                output.status().vm_status().major_status,
            );
            transaction_hash.push(txn_hash);
        }

        let (accumulator_root, first_leaf_idx) =
            self.accumulator.append(&transaction_hash).unwrap();
        assert_eq!(
            block.header().state_root(),
            state_root,
            "verify block:{:?} state_root fail.",
            block.header().id()
        );
        if let Err(e) = self.verify_proof(accumulator_root, &transaction_hash, first_leaf_idx) {
            warn!("err : {:?}", e);
        }
        self.save_block(&block);
        if let Err(e) = chain_state.flush() {
            warn!("err : {:?}", e);
        }
        self.chain_info.append(&block.header());
        self.head = block.clone();
        self.save_block_info(BlockInfo::new(
            header.id(),
            self.accumulator.get_frozen_subtree_roots().unwrap(),
            self.accumulator.num_leaves(),
            self.accumulator.num_nodes(),
        ));
        debug!("save block {:?} succ.", block.header().id());
        //todo
        Ok(())
    }

    fn chain_state(&mut self) -> &dyn ChainState {
        &self.chain_state
    }
}
