use std::sync::Arc;

use anyhow::Result;
use parking_lot::Mutex;
use tiny_adnl::utils::*;
use ton_block::Serializable;

use self::bridge::*;
use self::eth_subscriber::*;
use self::staking::*;
use self::ton_contracts::*;
use self::ton_subscriber::*;
use crate::config::*;
use crate::engine::state::State;
use crate::utils::*;

mod bridge;
mod eth_subscriber;
mod staking;
mod state;
mod ton_contracts;
mod ton_subscriber;

pub struct Engine {
    context: Arc<EngineContext>,
    bridge: Mutex<Option<Arc<Bridge>>>,
    staking: Mutex<Option<Arc<Staking>>>,
}

impl Engine {
    pub async fn new(
        config: RelayConfig,
        global_config: ton_indexer::GlobalConfig,
    ) -> Result<Arc<Self>> {
        let context = EngineContext::new(config, global_config).await?;

        Ok(Arc::new(Self {
            context,
            bridge: Mutex::new(None),
            staking: Mutex::new(None),
        }))
    }

    pub async fn start(&self) -> Result<()> {
        // Sync node and subscribers
        self.context.start().await?;

        // Fetch bridge configuration
        let bridge_account = only_account_hash(&self.context.settings.bridge_address);

        let bridge_contract = match self
            .context
            .ton_subscriber
            .get_contract_state(bridge_account)
            .await?
        {
            Some(contract) => contract,
            None => return Err(EngineError::BridgeAccountNotFound.into()),
        };

        let bridge_configuration = BridgeContract(&bridge_contract).bridge_configuration()?;

        // Initialize bridge
        let bridge = Bridge::new(self.context.clone(), bridge_account).await?;
        *self.bridge.lock() = Some(bridge);

        // Initialize staking
        let staking = Staking::new(self.context.clone(), bridge_configuration.staking).await?;
        *self.staking.lock() = Some(staking);

        // Done
        Ok(())
    }
}

pub struct EngineContext {
    pub settings: BridgeConfig,
    pub state: Arc<State>,
    pub ton_engine: Arc<ton_indexer::Engine>,
    pub ton_subscriber: Arc<TonSubscriber>,
    pub eth_subscribers: Arc<EthSubscriberRegistry>,
}

impl EngineContext {
    async fn new(
        config: RelayConfig,
        global_config: ton_indexer::GlobalConfig,
    ) -> Result<Arc<Self>> {
        let settings = config.bridge_settings;

        let state = State::new(&settings.db_path).await?;
        state.apply_migrations().await?;

        let ton_subscriber = TonSubscriber::new();
        let ton_engine = ton_indexer::Engine::new(
            config.node_settings,
            global_config,
            vec![ton_subscriber.clone() as Arc<dyn ton_indexer::Subscriber>],
        )
        .await?;

        let eth_subscribers =
            EthSubscriberRegistry::new(state.clone(), settings.networks.clone()).await?;

        Ok(Arc::new(Self {
            settings,
            state,
            ton_engine,
            ton_subscriber,
            eth_subscribers,
        }))
    }

    async fn start(&self) -> Result<()> {
        self.ton_engine.start().await?;
        self.ton_subscriber.start().await?;
        Ok(())
    }

    pub async fn get_all_shard_accounts(&self) -> Result<ShardAccountsMap> {
        let shard_blocks = self.ton_subscriber.wait_shards().await?.block_ids;

        let mut shard_accounts =
            FxHashMap::with_capacity_and_hasher(shard_blocks.len(), Default::default());
        for (shard_ident, block_id) in shard_blocks {
            let shard = self.ton_engine.wait_state(&block_id, None, false).await?;
            let accounts = shard.state().read_accounts()?;
            shard_accounts.insert(shard_ident, accounts);
        }

        Ok(shard_accounts)
    }

    pub async fn send_ton_message(&self, message: &ton_block::Message) -> Result<()> {
        let to = match message.header() {
            ton_block::CommonMsgInfo::ExtInMsgInfo(header) => {
                ton_block::AccountIdPrefixFull::prefix(&header.dst)?
            }
            _ => return Err(EngineError::ExternalTonMessageExpected.into()),
        };

        let cells = message.write_to_new_cell()?.into();
        let serialized = ton_types::serialize_toc(&cells)?;

        self.ton_engine
            .broadcast_external_message(&to, &serialized)
            .await
    }
}

#[derive(thiserror::Error, Debug)]
enum EngineError {
    #[error("External ton message expected")]
    ExternalTonMessageExpected,
    #[error("Bridge account not found")]
    BridgeAccountNotFound,
}
