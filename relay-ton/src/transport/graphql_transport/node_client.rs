use graphql_client::*;
use reqwest::Client;
use ton_block::{
    Account, AccountIdPrefixFull, AccountStuff, Block, Deserializable, ShardIdent, Transaction,
};

use crate::prelude::*;
use crate::transport::errors::*;
use crate::transport::utils::*;

#[derive(Clone)]
pub struct NodeClient {
    client: Client,
    endpoint: String,
}

impl NodeClient {
    pub fn new(client: &Client, endpoint: &str) -> Self {
        Self {
            client: client.clone(),
            endpoint: endpoint.to_owned(),
        }
    }

    async fn fetch<T>(&self, params: &T::Variables) -> TransportResult<T::ResponseData>
    where
        T: GraphQLQuery,
    {
        let response = self
            .client
            .post(&self.endpoint)
            .json(params)
            .send()
            .await
            .map_err(api_failure)?;

        response
            .json::<T::ResponseData>()
            .await
            .map_err(api_failure)
    }

    pub async fn get_account_state(&self, addr: &str) -> TransportResult<AccountStuff> {
        #[derive(GraphQLQuery)]
        #[graphql(
            schema_path = "src/transport/graphql_transport/schema.graphql",
            query_path = "src/transport/graphql_transport/query_account_state.graphql"
        )]
        struct QueryAccountState;

        let account_state = self
            .fetch::<QueryAccountState>(&query_account_state::Variables {
                address: addr.to_owned(),
            })
            .await?
            .accounts
            .ok_or_else(invalid_response)?
            .into_iter()
            .next()
            .and_then(|item| item.and_then(|account| account.boc))
            .ok_or_else(|| TransportError::AccountNotFound)?;

        match Account::construct_from_base64(&account_state) {
            Ok(Account::Account(account_stuff)) => Ok(account_stuff),
            Ok(_) => Err(TransportError::AccountNotFound),
            Err(e) => Err(TransportError::FailedToParseAccountState {
                reason: e.to_string(),
            }),
        }
    }

    pub async fn get_latest_block(&self, addr: &MsgAddressInt) -> TransportResult<String> {
        #[derive(GraphQLQuery)]
        #[graphql(
            schema_path = "src/transport/graphql_transport/schema.graphql",
            query_path = "src/transport/graphql_transport/query_latest_masterchain_block.graphql"
        )]
        struct QueryLatestMasterchainBlock;

        #[derive(GraphQLQuery)]
        #[graphql(
            schema_path = "src/transport/graphql_transport/schema.graphql",
            query_path = "src/transport/graphql_transport/query_node_se_conditions.graphql"
        )]
        struct QueryNodeSEConditions;

        #[derive(GraphQLQuery)]
        #[graphql(
            schema_path = "src/transport/graphql_transport/schema.graphql",
            query_path = "src/transport/graphql_transport/query_node_se_latest_block.graphql"
        )]
        struct QueryNodeSELatestBlock;

        let workchain_id = addr.get_workchain_id();

        let blocks = self
            .fetch::<QueryLatestMasterchainBlock>(&query_latest_masterchain_block::Variables)
            .await?
            .blocks
            .ok_or_else(no_blocks_found)?;

        match blocks.into_iter().flatten().next() {
            // Common case
            Some(block) => {
                // Handle simple case when searched account is in masterchain
                if workchain_id == -1 {
                    return block.id.ok_or_else(no_blocks_found);
                }

                // Find account's shard block
                let shards: Vec<_> = block
                    .master
                    .and_then(|master| master.shard_hashes)
                    .ok_or_else(no_blocks_found)?;

                // Find matching shard
                for item in shards.into_iter().flatten() {
                    match (item.workchain_id, item.shard) {
                        (Some(workchain_id), Some(shard)) => {
                            if check_shard_match(workchain_id, &shard, addr)? {
                                return item
                                    .descr
                                    .and_then(|descr| descr.root_hash)
                                    .ok_or_else(no_blocks_found);
                            }
                        }
                        _ => return Err(TransportError::NoBlocksFound),
                    }
                }

                Err(TransportError::NoBlocksFound)
            }
            // Check Node SE case (without masterchain and sharding)
            None => {
                let block = self
                    .fetch::<QueryNodeSEConditions>(&query_node_se_conditions::Variables {
                        workchain: workchain_id as i64,
                    })
                    .await?
                    .blocks
                    .and_then(|blocks| blocks.into_iter().flatten().next())
                    .ok_or_else(no_blocks_found)?;

                match (block.after_merge, &block.shard) {
                    (Some(after_merge), Some(shard))
                        if !after_merge && shard == "8000000000000000" => {}
                    // If workchain is sharded then it is not Node SE and missing masterchain blocks is error
                    _ => return Err(TransportError::NoBlocksFound),
                }

                self.fetch::<QueryNodeSELatestBlock>(&query_node_se_latest_block::Variables {
                    workchain: workchain_id as i64,
                })
                .await?
                .blocks
                .and_then(|blocks| {
                    blocks
                        .into_iter()
                        .flatten()
                        .next()
                        .and_then(|block| block.id)
                })
                .ok_or_else(no_blocks_found)
            }
        }
    }

    pub async fn get_account_transactions(
        &self,
        addr: &MsgAddressInt,
        last_trans_lt: u64,
        limit: i64,
    ) -> TransportResult<Vec<Transaction>> {
        #[derive(GraphQLQuery)]
        #[graphql(
            schema_path = "src/transport/graphql_transport/schema.graphql",
            query_path = "src/transport/graphql_transport/query_account_transactions.graphql"
        )]
        pub struct QueryAccountTransactions;

        self.fetch::<QueryAccountTransactions>(&query_account_transactions::Variables {
            address: addr.to_string(),
            last_transaction_lt: last_trans_lt.to_string(),
            limit,
        })
        .await?
        .transactions
        .ok_or_else(invalid_response)?
        .into_iter()
        .flatten()
        .map(|transaction| {
            let boc = transaction.boc.ok_or_else(invalid_response)?;
            Transaction::construct_from_base64(&boc).map_err(api_failure)
        })
        .collect::<Result<Vec<_>, _>>()
    }

    pub async fn wait_for_next_block(
        &self,
        current: &str,
        addr: &MsgAddressInt,
    ) -> TransportResult<String> {
        #[derive(GraphQLQuery)]
        #[graphql(
            schema_path = "src/transport/graphql_transport/schema.graphql",
            query_path = "src/transport/graphql_transport/query_next_block.graphql"
        )]
        pub struct QueryNextBlock;

        let block = self
            .fetch::<QueryNextBlock>(&query_next_block::Variables {
                id: current.to_owned(),
                timeout: 60.0, // todo: move into config
            })
            .await?
            .blocks
            .and_then(|blocks| blocks.into_iter().flatten().next())
            .ok_or_else(no_blocks_found)?;

        let workchain_id = block.workchain_id.ok_or_else(invalid_response)?;
        let shard = block.shard.as_ref().ok_or_else(invalid_response)?;

        match (
            block.id,
            block.after_split,
            check_shard_match(workchain_id, shard, addr)?,
        ) {
            (Some(block_id), Some(true), false) => {
                #[derive(GraphQLQuery)]
                #[graphql(
                    schema_path = "src/transport/graphql_transport/schema.graphql",
                    query_path = "src/transport/graphql_transport/query_block_after_split.graphql"
                )]
                pub struct QueryBlockAfterSplit;

                self.fetch::<QueryBlockAfterSplit>(&query_block_after_split::Variables {
                    block_id,
                    prev_id: current.to_owned(),
                    timeout: 60.0, // todo: move into config
                })
                .await?
                .blocks
                .and_then(|block| block.into_iter().flatten().next())
                .ok_or_else(no_blocks_found)?
                .id
                .ok_or_else(invalid_response)
            }
            (Some(block_id), _, _) => Ok(block_id),
            _ => Err(invalid_response()),
        }
    }

    pub async fn get_block(&self, id: &str) -> TransportResult<Block> {
        #[derive(GraphQLQuery)]
        #[graphql(
            schema_path = "src/transport/graphql_transport/schema.graphql",
            query_path = "src/transport/graphql_transport/query_block.graphql"
        )]
        pub struct QueryBlock;

        let boc = self
            .fetch::<QueryBlock>(&query_block::Variables { id: id.to_owned() })
            .await?
            .blocks
            .and_then(|block| block.into_iter().flatten().next())
            .ok_or_else(no_blocks_found)?
            .boc
            .ok_or_else(invalid_response)?;

        Block::construct_from_base64(&boc).map_err(|e| TransportError::FailedToParseBlock {
            reason: e.to_string(),
        })
    }
}

fn check_shard_match(
    workchain_id: i64,
    shard: &str,
    addr: &MsgAddressInt,
) -> TransportResult<bool> {
    let shard = u64::from_str_radix(&shard, 16).map_err(|_| TransportError::NoBlocksFound)?;

    let ident = ShardIdent::with_tagged_prefix(workchain_id as i32, shard).map_err(api_failure)?;

    Ok(ident.contains_full_prefix(&AccountIdPrefixFull::prefix(addr).map_err(api_failure)?))
}

fn api_failure<T>(e: T) -> TransportError
where
    T: std::fmt::Display,
{
    TransportError::ApiFailure {
        reason: e.to_string(),
    }
}

fn invalid_response() -> TransportError {
    TransportError::ApiFailure {
        reason: "invalid response".to_owned(),
    }
}

fn no_blocks_found() -> TransportError {
    TransportError::NoBlocksFound
}
