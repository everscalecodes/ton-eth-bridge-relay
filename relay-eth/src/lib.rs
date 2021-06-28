#![deny(clippy::unwrap_used)]

use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Error, Result};
use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};
use sled::{Db, Tree};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::sync::RwLock;
use tokio::sync::Semaphore;
use tokio_stream::Stream;
use tryhard::backoff_strategies::{ExponentialBackoff, FixedBackoff};
use tryhard::{NoOnRetry, RetryFutureConfig};
use url::Url;
use web3::transports::http::Http;
pub use web3::types::SyncState;
pub use web3::types::{Address, BlockNumber, H256};
pub use web3::types::{FilterBuilder, Log, H160};
use web3::{Transport, Web3};

use relay_utils::retry;

const ETH_TREE_NAME: &str = "ethereum_data";
const ETH_LAST_MET_HEIGHT: &str = "last_met_height";

#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
pub struct Timeouts {
    pub get_eth_data_timeout: Duration,
    pub get_eth_data_attempts: u32,
    pub maximum_failed_responses_time: Duration,
    pub eth_poll_interval: Duration,
}

pub struct EthListener {
    web3: Web3<Http>,
    db: Tree,
    topics: Arc<RwLock<(HashSet<Address>, HashSet<H256>)>>,
    current_block: Arc<AtomicU64>,
    connections_pool: Arc<Semaphore>,
    relay_keys_function_to_topic_map: HashMap<String, H256>,
    timeouts: Timeouts,
    bridge_address: Address,
}

async fn get_actual_eth_height<T: Transport>(
    w3: &Web3<T>,
    connection_pool: &Arc<Semaphore>,
    get_eth_data_timeout: Duration,
) -> Result<u64> {
    use tokio::time::timeout;
    log::debug!("Getting height");
    let _permission = connection_pool.acquire().await;
    match timeout(get_eth_data_timeout, w3.eth().block_number()).await {
        Ok(a) => match a {
            Ok(a) => {
                let height = a.as_u64();
                log::debug!("Got height: {}", height);
                Ok(height)
            }
            Err(e) => {
                if let web3::error::Error::Transport(e) = &e {
                    if e == "hyper::Error(IncompleteMessage)" {
                        anyhow::bail!("Failed getting height: {}", e);
                    }
                }
                anyhow::bail!("Failed getting block number: {:?}", e);
            }
        },
        Err(e) => {
            anyhow::bail!("Timed out on getting actual eth block number: {:?}", e);
        }
    }
}

/// Returns topic hash and abi for ETH
pub fn parse_eth_abi(abi: &str) -> Result<HashMap<String, H256>, Error> {
    #[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Abi {
        pub inputs: Vec<Input>,
        pub name: String,
    }

    #[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Input {
        #[serde(rename = "type")]
        pub type_field: String,
    }

    let abis: Vec<Abi> = serde_json::from_str(abi)?;
    let mut topics = HashMap::with_capacity(abis.len());
    for abi in abis {
        let fn_name = abi.name;

        let input_types: String = abi
            .inputs
            .iter()
            .map(|x| x.type_field.clone())
            .collect::<Vec<String>>()
            .join(",");

        let signature = format!("{}({})", fn_name, input_types);
        topics.insert(
            fn_name,
            H256::from_slice(&*Keccak256::digest(signature.as_bytes())),
        );
    }

    Ok(topics)
}

#[derive(Debug)]
pub enum SyncedHeight {
    Synced(u64),
    NotSynced(u64),
}

impl SyncedHeight {
    pub fn as_u64(&self) -> u64 {
        match *self {
            SyncedHeight::Synced(a) => a,
            SyncedHeight::NotSynced(a) => a,
        }
    }
}

impl EthListener {
    //todo move to  config
    pub async fn new(
        url: Url,
        db: Db,
        connections_number: usize,
        timeouts: Timeouts,
        bridge_address: Address,
    ) -> Result<Self, Error> {
        let connection = Http::new(url.as_str()).expect("Failed connecting to ethereum node");
        log::info!("Connected to: {}", &url);
        let tree = db.open_tree(ETH_TREE_NAME)?;
        let web3 = Web3::new(connection);
        let current_block = Self::get_block_number_on_start(&tree, &web3).await?;
        let relay_keys_abi = parse_eth_abi(include_str!(
            "../abi/contracts_DistributedOwnable_sol_DistributedOwnable.json"
        ))?;
        let listener = Self {
            web3,
            db: tree,
            topics: Arc::new(Default::default()),
            connections_pool: Arc::new(Semaphore::new(connections_number)),
            current_block: Arc::new(AtomicU64::new(current_block)),
            relay_keys_function_to_topic_map: relay_keys_abi,
            timeouts,
            bridge_address,
        };
        // dbg!(listener.get_actual_keys().await?); //todo use it
        Ok(listener)
    }

    pub async fn start(self: Arc<Self>) -> Result<impl Stream<Item = Result<Event, Error>>, Error> {
        log::debug!("Started iterating over ethereum blocks.");
        let from_height = self.current_block.clone();
        let events_rx = self.spawn_blocks_scanner(from_height);
        Ok(events_rx)
    }

    pub fn change_eth_height(&self, height: u64) -> Result<(), Error> {
        self.current_block.store(height, Ordering::SeqCst);
        update_height(&self.db, height)?;
        Ok(())
    }

    pub async fn get_block_number_on_start(db: &Tree, web3: &Web3<Http>) -> Result<u64, Error> {
        Ok(match db.get(ETH_LAST_MET_HEIGHT)? {
            Some(a) => u64::from_le_bytes(a.as_ref().try_into()?),
            None => web3.eth().block_number().await?.as_u64(),
        })
    }

    pub async fn check_transaction(&self, hash: H256, event_index: u32) -> Result<Event, Error> {
        loop {
            // Trying to get data. Retrying in case of error
            let _permission = self.connections_pool.acquire().await;

            match retry(
                || self.web3.eth().transaction_receipt(hash),
                generate_default_timeout_config(self.timeouts.maximum_failed_responses_time),
                "get transaction receipt",
            )
            .await
            {
                Ok(a) => {
                    return match a {
                        //if no tx with this hash
                        None => Err(anyhow!("No transactions found by hash. Assuming it's fake")),
                        Some(a) => {
                            // if tx status is failed, then no such tx exists
                            match a.status {
                                Some(a) => {
                                    if a.as_u64() == 0 {
                                        return Err(anyhow!("Tx has failed status"));
                                    }
                                }
                                None => return Err(anyhow!("No status field in eth node answer")),
                            };

                            let logs = a.logs;
                            //parsing logs into events
                            let events: Result<Vec<_>, _> =
                                logs.into_iter().map(EthListener::log_to_event).collect();

                            let events = match events {
                                Ok(a) => a,
                                Err(e) => {
                                    log::error!(
                                        "No events for tx. Assuming confirmation is fake.: {}",
                                        e
                                    );
                                    return Err(anyhow!(
                                        "No events for tx. Assuming confirmation is fake.: {}",
                                        e
                                    ));
                                }
                            };
                            // if any event matches
                            let event: Option<_> = events
                                .into_iter()
                                .find(|x| x.tx_hash == hash && x.event_index == event_index);
                            match event {
                                Some(a) => Ok(a),
                                None => Err(anyhow!(
                                    "No events for tx. Assuming confirmation is fake.: {}"
                                )),
                            }
                        }
                    };
                }
                Err(e) => {
                    panic!(
                        "Failed fetching info from eth node in {:?}. Last error: {}",
                        self.timeouts.maximum_failed_responses_time, e
                    );
                }
            }
        }
    }

    async fn get_actual_keys(&self) -> Result<(Address, Vec<H160>), Error> {
        let address = self.bridge_address;
        async fn get_keys(
            topic: H256,
            address: Address,
            web3: &Web3<Http>,
        ) -> Result<HashSet<H160>, Error> {
            let filter = FilterBuilder::default()
                .address(vec![address])
                .topics(Some(vec![topic]), None, None, None)
                .from_block(BlockNumber::Earliest)
                .to_block(BlockNumber::Latest)
                .build();

            Ok(web3
                .eth()
                .logs(filter)
                .await?
                .into_iter()
                .map(EthListener::log_to_event)
                .filter_map(|x| match x {
                    Ok(a) if !a.data.is_empty() => {
                        match ethabi::decode(&[ethabi::ParamType::Address], &*a.data) {
                            Ok(a) => {
                                if a.is_empty() {
                                    log::error!("No addresses in data");
                                    None
                                } else {
                                    Some(
                                        a.first()
                                            .expect("Checked upper")
                                            .clone()
                                            .into_address()
                                            .map(|x| H160::from(x.0)),
                                    )
                                }
                            }
                            Err(e) => {
                                log::error!("Failed decoding data as address: {}", e);
                                None
                            }
                        }
                    }
                    Err(e) => {
                        log::error!("Failed parsing log as event: {}", e);
                        None
                    }
                    Ok(a) => {
                        log::error!("Bad data len: {}", a.data.len());
                        None
                    }
                })
                .flatten()
                .collect())
        }
        let ok_topic = self.relay_keys_function_to_topic_map["OwnershipGranted"];
        let bad_topic = self.relay_keys_function_to_topic_map["OwnershipRemoved"];
        // use hex::ToHex;
        let ok_fut = get_keys(ok_topic, address, &self.web3);
        let bad_fut = get_keys(bad_topic, address, &self.web3);
        let (ok, bad): (HashSet<_>, HashSet<_>) = futures::try_join!(ok_fut, bad_fut)?;
        Ok((address, ok.difference(&bad).cloned().collect()))
    }

    fn log_to_event(log: Log) -> Result<Event, Error> {
        let data = log.data.0;
        let hash = match log.transaction_hash {
            Some(a) => a,
            None => {
                log::error!("No tx hash!");
                return Err(Error::msg("No tx hash in log"));
            }
        };
        let block_number = match log.block_number {
            Some(a) => a.as_u64(),
            None => {
                let err = "No block number in log!".to_string();
                log::error!("{}", &err);
                return Err(Error::msg(err));
            }
        };
        let event_index = match log.log_index {
            Some(a) => a.as_u32(),
            None => {
                let err = format!(
                    "No transaction_log_index in log. Tx hash: {}. Block: {}",
                    hash, block_number
                );
                log::warn!("{}", &err);
                0
            }
        };
        let block_hash = match log.block_hash {
            Some(a) => a,
            None => {
                let err = format!("No hash in log. Tx hash: {}. Block: {}", hash, block_number);
                log::error!("{}", err);
                return Err(Error::msg(err));
            }
        };

        log::debug!("Sent logs from block {} with hash {}", block_number, hash);
        Ok(Event {
            address: log.address,
            data,
            tx_hash: hash,
            topics: log.topics,
            event_index,
            block_number,
            block_hash,
        })
    }

    ///subscribe on address and topic
    pub async fn add_topic(&self, address: Address, topic: H256) {
        log::info!(
            "Subscribing for address: {:?} with topic: {:?}",
            address,
            topic
        );

        let mut topics = self.topics.write().await;
        topics.0.insert(address);
        topics.1.insert(topic);
    }

    ///unsubscribe from address
    pub async fn unsubscribe_from_address(&self, address: &Address) {
        let mut topics = self.topics.write().await;
        topics.0.remove(address);
    }

    ///unsubscribe from 1 topic
    pub async fn unsubscribe_from_topic(&self, topic: &H256) {
        let mut topics = self.topics.write().await;
        topics.1.remove(topic);
    }

    ///unsubscribe from list of topics
    pub async fn unsubscribe_from_topics(&self, topics_list: &[H256]) {
        let mut topics = self.topics.write().await;
        topics_list.iter().for_each(|t| {
            topics.1.remove(t);
        });
    }

    pub async fn get_synced_height(&self) -> Result<SyncedHeight, Error> {
        match retry(
            || self.web3.eth().syncing(),
            RetryFutureConfig::new(self.timeouts.get_eth_data_attempts)
                .fixed_backoff(self.timeouts.get_eth_data_timeout),
            "eth syncing status",
        )
        .await
        {
            Ok(sync_state) => match sync_state {
                SyncState::Syncing(a) => {
                    let current_synced_block = a.current_block.as_u64();
                    let network_height = a.highest_block.as_u64();
                    if network_height - current_synced_block > 200 {
                        log::warn!(
                            "Ethereum node is far behind network head: {} blocks to sync",
                            network_height - current_synced_block
                        );
                        log::warn!("{:?}", a);
                    }
                    Ok(SyncedHeight::NotSynced(current_synced_block))
                }
                SyncState::NotSyncing => match retry(
                    || {
                        get_actual_eth_height(
                            &self.web3,
                            &self.connections_pool,
                            self.timeouts.get_eth_data_timeout,
                        )
                    },
                    generate_default_timeout_config(self.timeouts.maximum_failed_responses_time),
                    "get actual ethereum height",
                )
                .await
                {
                    Ok(a) => Ok(SyncedHeight::Synced(a)),
                    Err(e) => panic!(
                        "Failed getting answer from eth node. Last error: {}. Elapsed time: {:?}",
                        e, self.timeouts.maximum_failed_responses_time
                    ),
                },
            },
            Err(e) => {
                anyhow::bail!("Failed fetching eth sync status: {}", e);
            }
        }
    }

    fn spawn_blocks_scanner(
        self: Arc<Self>,
        from_height: Arc<AtomicU64>,
    ) -> impl Stream<Item = Result<Event, Error>> {
        let (events_tx, events_rx) = unbounded_channel();

        tokio::spawn({
            let connection_pool = self.connections_pool.clone();
            let scanned_height = from_height;
            let this = self;
            async move {
                loop {
                    // trying to get actual height
                    let ethereum_actual_height = match retry(
                        || {
                            get_actual_eth_height(
                                &this.web3,
                                &connection_pool,
                                this.timeouts.get_eth_data_timeout,
                            )
                        },
                        generate_fixed_config(
                            this.timeouts.eth_poll_interval,
                            this.timeouts.maximum_failed_responses_time,
                        ),
                        "get actual ethereum height",
                    )
                    .await
                    {
                        Ok(a) => a,
                        Err(e) => {
                            panic!("Failed getting actual ethereum height: {}", e);
                        }
                    };

                    let mut loaded_height = scanned_height.load(Ordering::SeqCst);
                    // sleeping in case of synchronization with eth
                    if loaded_height >= ethereum_actual_height {
                        tokio::time::sleep(this.timeouts.eth_poll_interval).await;
                        continue;
                    }
                    // batch processing all blocks from `loaded_height` to `ethereum_actual_height`
                    else {
                        log::debug!(
                            "Batch processing blocks from {} to {}",
                            loaded_height,
                            ethereum_actual_height
                        );
                        let block_number = BlockNumber::from(loaded_height);
                        this.process_block(
                            block_number,
                            BlockNumber::from(ethereum_actual_height),
                            &events_tx,
                        )
                        .await;
                        loaded_height = ethereum_actual_height;
                    }

                    if let Err(e) = update_eth_state(&this.db, loaded_height, ETH_LAST_MET_HEIGHT) {
                        log::error!("Critical error: failed saving eth state: {}", e);
                    };

                    scanned_height.store(loaded_height, Ordering::SeqCst);
                    log::trace!("Scanned height: {}", scanned_height.load(Ordering::SeqCst));

                    tokio::time::sleep(this.timeouts.eth_poll_interval).await;
                }
            }
        });
        tokio_stream::wrappers::UnboundedReceiverStream::new(events_rx)
    }

    async fn process_block(
        &self,
        from: BlockNumber,
        to: BlockNumber,
        events_tx: &UnboundedSender<Result<Event, Error>>,
    ) {
        // TODO: optimize
        let (addresses, topics): (Vec<_>, Vec<_>) = {
            let state = self.topics.read().await;
            (
                state.0.iter().cloned().collect(),
                state.1.iter().cloned().collect(),
            )
        };
        if addresses.is_empty() && topics.is_empty() {
            log::warn!("Addresses and topics are empty. Cowardly refusing to process all ethereum transactions");
            return;
        }
        let filter = FilterBuilder::default()
            .address(addresses)
            .topics(Some(topics), None, None, None)
            .from_block(from)
            .to_block(to)
            .build();

        let _permit = self.connections_pool.acquire().await;
        match retry(
            || self.web3.eth().logs(filter.clone()),
            generate_default_timeout_config(self.timeouts.maximum_failed_responses_time),
            "get contract logs",
        )
        .await
        {
            Ok(a) => {
                if !a.is_empty() {
                    log::info!("There are some logs in block: {:?}", from);
                }
                for log in a {
                    let event = EthListener::log_to_event(log);
                    match event {
                        Ok(a) => {
                            if let Err(e) = events_tx.send(Ok(a)) {
                                log::error!("FATAL ERROR. Failed sending event: {:?}", e);
                            }
                            continue;
                        }
                        Err(e) => {
                            log::error!("Failed parsing log to event: {:?}", e);
                            if let Err(e) = events_tx.send(Err(e)) {
                                log::error!("Failed sending event: {:?}", e);
                            }
                            continue;
                        }
                    }
                }
            }
            Err(e) => {
                panic!(
                    "Failed getting answer from eth node. Last error: {}. Elapsed time: {:?}",
                    e, self.timeouts.maximum_failed_responses_time
                )
            }
        };
    }
}

///topics: `Keccak256("Method_Signature")`
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Serialize, Deserialize, Ord)]
pub struct Event {
    pub address: Address,
    pub data: Vec<u8>,
    pub tx_hash: H256,
    pub topics: Vec<H256>,
    pub event_index: u32,
    pub block_number: u64,
    pub block_hash: H256,
}

fn update_eth_state(db: &Tree, height: u64, key: &str) -> Result<(), Error> {
    db.insert(key, &height.to_le_bytes())?;
    Ok(())
}

pub fn update_height(db: &Tree, height: u64) -> Result<(), Error> {
    update_eth_state(&db, height, ETH_LAST_MET_HEIGHT)?;
    Ok(())
}

#[inline]
fn generate_default_timeout_config(
    total_time: Duration,
) -> RetryFutureConfig<ExponentialBackoff, NoOnRetry> {
    let max_delay = Duration::from_secs(600);
    let times = relay_utils::calculate_times_from_max_delay(
        Duration::from_secs(1),
        2f64,
        max_delay,
        total_time,
    );
    tryhard::RetryFutureConfig::new(times)
        .exponential_backoff(Duration::from_secs(1))
        .max_delay(Duration::from_secs(600))
}

fn generate_fixed_config(
    sleep_time: Duration,
    total_time: Duration,
) -> RetryFutureConfig<FixedBackoff, NoOnRetry> {
    let times = (total_time.as_secs() / sleep_time.as_secs())
        .try_into()
        .expect("Overflow");
    tryhard::RetryFutureConfig::new(times).fixed_backoff(sleep_time)
}
