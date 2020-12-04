use anyhow::Error;
use bincode::{deserialize, serialize};
use futures::Stream;
use futures::StreamExt;
use sled::{Db, Tree};
use tokio::sync::mpsc::UnboundedReceiver;

use relay_ton::contracts::{
    ContractWithEvents, EthereumEventConfigurationContract,
    EthereumEventConfigurationContractEvent, EthereumEventContract, EthereumEventDetails,
};
use relay_ton::prelude::{Arc, BigUint};
use relay_ton::transport::Transport;

use crate::engine::bridge::ton_config_listener::ExtendedEventInfo;

const PERSISTENT_TREE_NAME: &str = "unconfirmed_events";

pub struct TonWatcher {
    db: Tree,
    contract_configuration: Arc<EthereumEventConfigurationContract>,
    transport: Arc<dyn Transport>,
}

impl TonWatcher {
    pub fn new(
        db: Db,
        contract_configuration: Arc<EthereumEventConfigurationContract>,
        transport: Arc<dyn Transport>,
    ) -> Result<Self, Error> {
        Ok(Self {
            db: db.open_tree(PERSISTENT_TREE_NAME)?,
            contract_configuration,
            transport,
        })
    }

    pub async fn watch(&self, events: UnboundedReceiver<ExtendedEventInfo>) {
        let db = &self.db;
        let mut events = events;
        while let Some(event) = events.next().await {
            let tx_hash = &event.data.ethereum_event_transaction;
            db.insert(tx_hash, serialize(&event).expect("Shouldn't fail"));
        }
    }

    pub fn drop_key(&self, key: &[u8]) -> Result<(), Error> {
        self.db.remove(key)?;
        Ok(())
    }

    pub fn get_event(&self, key: &[u8]) -> Result<Option<ExtendedEventInfo>, Error> {
        Ok(self
            .db
            .get(key)?
            .and_then(|x| deserialize(&x).expect("Shouldn't fail")))
    }

    pub fn scan_for_block(&self, block_number: BigUint) -> Vec<ExtendedEventInfo> {
        self.db
            .iter()
            .values()
            .filter_map(|x| match x {
                Ok(a) => Some(a),
                Err(e) => {
                    log::error!("Bad value in {}: {}", PERSISTENT_TREE_NAME, e);
                    None
                }
            })
            .map(|x| deserialize::<ExtendedEventInfo>(&x))
            .filter_map(|x| x.ok()) // shouldn't fail
            .filter(|x| x.data.event_block_number == block_number)
            .collect()
    }
}
