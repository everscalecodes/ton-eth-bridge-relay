use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use nekoton_abi::UnpackAbiPlain;
use parking_lot::Mutex;
use tokio::sync::futures::Notified;
use tokio::sync::mpsc;
use tokio::sync::Notify;
use ton_types::UInt256;

use crate::engine::keystore::*;
use crate::engine::ton_contracts::*;
use crate::engine::ton_subscriber::*;
use crate::engine::EngineContext;
use crate::utils::*;

pub struct Staking {
    context: Arc<EngineContext>,

    current_relay_round: Mutex<RoundState>,
    relay_round_started_notify: Notify,
    elections_start_notify: Notify,
    elections_end_notify: Notify,
    election_timings_changed_notify: Notify,

    staking_account: UInt256,
    staking_observer: Arc<AccountObserver<(RoundState, StakingEvent)>>,
    user_data_account: UInt256,
    user_data_observer: Arc<AccountObserver<UserDataEvent>>,
}

impl Staking {
    pub async fn new(ctx: Arc<EngineContext>, staking_account: UInt256) -> Result<Arc<Self>> {
        // Prepare staking
        ctx.ensure_user_data_verified(staking_account)
            .await
            .context("Failed to ensure that user data is verified")?;

        // Prepare initial data
        let shard_accounts = ctx.get_all_shard_accounts().await?;
        let staking_contract = shard_accounts
            .find_account(&staking_account)?
            .context("Staking contract not found")?;
        let staking_contract = StakingContract(&staking_contract);

        let current_relay_round = staking_contract
            .get_round_state()
            .context("Current relay round not found")?;

        let user_data_account = staking_contract
            .get_user_data_address(&ctx.staker_account)
            .context("User data account not found")?;

        let should_vote = match &current_relay_round.elections_state {
            ElectionsState::Started { .. } => {
                let elections_contract = shard_accounts
                    .find_account(&current_relay_round.next_elections_account)?
                    .context("Next elections contract not found")?;
                let elections_contract = ElectionsContract(&elections_contract);
                let elected = elections_contract
                    .staker_addrs()
                    .context("Failed to get staker addresses")?
                    .contains(&ctx.staker_account);
                !elected
            }
            _ => false,
        };

        let (staking_events_tx, staking_events_rx) = mpsc::unbounded_channel();
        let (user_data_events_tx, user_data_events_rx) = mpsc::unbounded_channel();

        // Create object
        let staking = Arc::new(Self {
            context: ctx,
            current_relay_round: Mutex::new(current_relay_round),
            relay_round_started_notify: Default::default(),
            elections_start_notify: Default::default(),
            elections_end_notify: Default::default(),
            election_timings_changed_notify: Default::default(),
            staking_account,
            staking_observer: AccountObserver::new(&staking_events_tx),
            user_data_account,
            user_data_observer: AccountObserver::new(&user_data_events_tx),
        });

        start_listening_events(
            &staking,
            "StakingContract",
            staking_events_rx,
            Self::process_staking_event,
        );
        start_listening_events(
            &staking,
            "UserDataContract",
            user_data_events_rx,
            Self::process_user_data_event,
        );

        let context = &staking.context;
        context
            .ton_subscriber
            .add_transactions_subscription([staking_account], &staking.staking_observer);
        context
            .ton_subscriber
            .add_transactions_subscription([user_data_account], &staking.user_data_observer);

        if should_vote {
            staking.become_relay_next_round().await?;
        }

        // TODO: get all shard states and collect reward

        staking.start_managing_elections();

        Ok(staking)
    }

    async fn process_staking_event(
        self: Arc<Self>,
        (_, (round_state, event)): (UInt256, (RoundState, StakingEvent)),
    ) -> Result<()> {
        let mut current_relay_round = self.current_relay_round.lock();
        *current_relay_round = round_state;

        match event {
            StakingEvent::ElectionStarted(_) => {
                self.elections_start_notify.notify_waiters();

                let staking = self.clone();
                tokio::spawn(async move {
                    let notify_fut = staking.elections_end_notify.notified();
                    tokio::select! {
                        result = staking.become_relay_next_round() => {
                            if let Err(e) = result {
                                log::error!("Failed to become relay next round: {:?}", e);
                            }
                        },
                        _ = notify_fut => {
                            log::warn!("Early exit from become_relay_next_round due to the elections end");
                        }
                    }
                });
            }
            StakingEvent::ElectionEnded(_) => {
                self.elections_end_notify.notify_waiters();
            }
            StakingEvent::RelayRoundInitialized(event) => {
                self.relay_round_started_notify.notify_waiters();

                let staking = self.clone();
                tokio::spawn(async move {
                    const ROUND_OFFSET: u64 = 10; // seconds

                    let now = chrono::Utc::now().timestamp() as u64;
                    tokio::time::sleep(Duration::from_secs(
                        (event.round_end_time as u64).saturating_sub(now) + ROUND_OFFSET,
                    ))
                    .await;

                    if let Err(e) = staking.get_reward_for_relay_round(event.round_num).await {
                        log::error!(
                            "Failed to collect reward for round {}: {:?}",
                            event.round_num,
                            e
                        );
                    }
                });
            }
            StakingEvent::RelayConfigUpdated(_) => {
                self.election_timings_changed_notify.notify_waiters();
            }
        }

        Ok(())
    }

    async fn process_user_data_event(
        self: Arc<Self>,
        (_, event): (UInt256, UserDataEvent),
    ) -> Result<()> {
        // TODO: handle?

        match event {
            UserDataEvent::RelayMembershipRequested(_) => {}
            UserDataEvent::TonPubkeyConfirmed(_) => {}
            UserDataEvent::EthAddressConfirmed(_) => {}
        }
        Ok(())
    }

    fn start_managing_elections(self: &Arc<Self>) {
        let staking = Arc::downgrade(self);

        tokio::spawn(async move {
            loop {
                let staking = match staking.upgrade() {
                    Some(staking) => staking,
                    None => return,
                };

                let (elections_state, timings_changed_fut) = {
                    let current_relay_round = staking.current_relay_round.lock();
                    let elections_state = current_relay_round.elections_state;
                    log::info!("Elections management loop. State: {:?}", elections_state);

                    let elections_state = match elections_state {
                        ElectionsState::NotStarted { start_time } => {
                            PendingElectionsState::NotStarted {
                                start_time,
                                inner_fut: staking.elections_start_notify.notified(),
                                outer_fut: staking.elections_start_notify.notified(),
                            }
                        }
                        ElectionsState::Started { end_time, .. } => {
                            PendingElectionsState::Started {
                                end_time,
                                inner_fut: staking.elections_end_notify.notified(),
                                outer_fut: staking.elections_end_notify.notified(),
                            }
                        }
                        ElectionsState::Finished => PendingElectionsState::Finished {
                            new_round_fut: staking.relay_round_started_notify.notified(),
                        },
                    };
                    let timings_changed_fut = staking.election_timings_changed_notify.notified();

                    (elections_state, timings_changed_fut)
                };

                let now = chrono::Utc::now().timestamp() as u64;
                log::info!("Now: {}", now);

                match elections_state {
                    PendingElectionsState::NotStarted {
                        start_time,
                        inner_fut,
                        outer_fut,
                    } => {
                        let staking = staking.clone();
                        let action = async move {
                            let delay = (start_time as u64).saturating_sub(now);

                            log::info!("Starting elections in {} seconds", delay);
                            tokio::time::sleep(Duration::from_secs(delay)).await;

                            log::info!("Starting elections");
                            if let Err(e) = staking.start_election().await {
                                log::error!("Failed to start election: {:?}", e);
                            }

                            log::info!("Waiting elections start");
                            inner_fut.await;
                        };

                        tokio::select! {
                            _ = action => continue,
                            _ = outer_fut => {
                                log::warn!("Elections loop: cancelling elections start. Already started");
                            }
                            _ = timings_changed_fut => {
                                log::warn!("Elections loop: cancelling elections start. Timings changed");
                            }
                        }
                    }
                    PendingElectionsState::Started {
                        end_time,
                        inner_fut,
                        outer_fut,
                    } => {
                        let staking = staking.clone();
                        let action = async move {
                            let delay = (end_time as u64).saturating_sub(now);

                            log::info!("Ending elections in {} seconds", delay);
                            tokio::time::sleep(Duration::from_secs(delay)).await;

                            log::info!("Ending elections");
                            if let Err(e) = staking.end_election().await {
                                log::error!("Failed to end election: {:?}", e);
                            }

                            log::info!("Waiting elections end");
                            inner_fut.await;
                        };

                        tokio::select! {
                            _ = action => continue,
                            _ = outer_fut => {
                                log::warn!("Elections loop: cancelling elections ending. Already ended");
                            }
                            _ = timings_changed_fut => {
                                log::warn!("Elections loop: cancelling elections ending. Timings changed");
                            }
                        }
                    }
                    PendingElectionsState::Finished { new_round_fut } => {
                        log::info!("Elections loop: waiting new round");
                        new_round_fut.await
                    }
                }
            }
        });
    }

    async fn become_relay_next_round(&self) -> Result<()> {
        self.context
            .deliver_message(
                self.user_data_observer.clone(),
                UnsignedMessage::new(
                    user_data_contract::become_relay_next_round(),
                    self.user_data_account,
                ),
            )
            .await
    }

    async fn get_reward_for_relay_round(&self, relay_round: u32) -> Result<()> {
        self.context
            .deliver_message(
                self.user_data_observer.clone(),
                UnsignedMessage::new(
                    user_data_contract::get_reward_for_relay_round(),
                    self.user_data_account,
                )
                .arg(relay_round),
            )
            .await
    }

    async fn start_election(&self) -> Result<()> {
        self.context
            .deliver_message(
                self.staking_observer.clone(),
                UnsignedMessage::new(
                    staking_contract::start_election_on_new_round(),
                    self.staking_account,
                ),
            )
            .await
    }

    async fn end_election(&self) -> Result<()> {
        self.context
            .deliver_message(
                self.staking_observer.clone(),
                UnsignedMessage::new(staking_contract::end_election(), self.staking_account),
            )
            .await
    }
}

impl EngineContext {
    async fn ensure_user_data_verified(self: &Arc<Self>, staking_account: UInt256) -> Result<()> {
        let shard_accounts = self.get_all_shard_accounts().await?;
        let staking_contract = shard_accounts
            .find_account(&staking_account)?
            .context("Staking contract not found")?;
        let staking_contract = StakingContract(&staking_contract);

        // Get bridge ETH event configuration
        let bridge_event_configuration =
            staking_contract.get_eth_bridge_configuration_details(&shard_accounts)?;
        log::info!(
            "Bridge event configuration: {:?}",
            bridge_event_configuration
        );

        // Initialize user data
        let user_data_account = staking_contract.get_user_data_address(&self.staker_account)?;
        log::info!("User data account: {:x}", user_data_account);
        let user_data_contract = shard_accounts
            .find_account(&user_data_account)?
            .context("User data account not found")?;
        let user_data_contract = UserDataContract(&user_data_contract);

        user_data_contract
            .ensure_verified(self, user_data_account, bridge_event_configuration)
            .await
    }
}

impl UserDataContract<'_> {
    async fn ensure_verified(
        &self,
        context: &Arc<EngineContext>,
        user_data_account: UInt256,
        bridge_event_configuration: EthEventConfigurationDetails,
    ) -> Result<()> {
        let ton_pubkey_confirmed_notify = Arc::new(Notify::new());
        let eth_address_confirmed_notify = Arc::new(Notify::new());

        let ton_notified = ton_pubkey_confirmed_notify.notified();
        let eth_notified = eth_address_confirmed_notify.notified();

        let (user_data_events_tx, mut user_data_events_rx) =
            mpsc::unbounded_channel::<(UInt256, UserDataEvent)>();

        let details = self
            .get_details()
            .context("Failed to get UserData details")?;
        log::info!("UserData details: {:?}", details);

        let relay_eth_address = *context.keystore.eth.address().as_fixed_bytes();
        let relay_ton_pubkey = *context.keystore.ton.public_key();

        if details.relay_eth_address != relay_eth_address {
            return Err(StakingError::UserDataEthAddressMismatch.into());
        }
        if details.relay_ton_pubkey != relay_ton_pubkey {
            return Err(StakingError::UserDataTonPublicKeyMismatch.into());
        }

        let user_data_observer = AccountObserver::new(&user_data_events_tx);

        tokio::spawn({
            let ton_pubkey_confirmed_notify = ton_pubkey_confirmed_notify.clone();
            let eth_address_confirmed_notify = eth_address_confirmed_notify.clone();

            async move {
                while let Some((_, event)) = user_data_events_rx.recv().await {
                    match event {
                        UserDataEvent::TonPubkeyConfirmed(event) => {
                            if event.ton_pubkey == relay_ton_pubkey {
                                log::info!("Received TON pubkey confirmation");
                                ton_pubkey_confirmed_notify.notify_waiters();
                            } else {
                                log::error!("Confirmed TON pubkey mismatch");
                            }
                        }
                        UserDataEvent::EthAddressConfirmed(event) => {
                            if event.eth_addr == relay_eth_address {
                                log::info!("Received ETH address confirmation");
                                eth_address_confirmed_notify.notify_waiters();
                            } else {
                                log::error!("Confirmed ETH address mismatch");
                            }
                        }
                        UserDataEvent::RelayMembershipRequested(_) => { /* do nothing */ }
                    }
                }
            }
        });

        context
            .ton_subscriber
            .add_transactions_subscription([user_data_account], &user_data_observer);

        if details.ton_pubkey_confirmed {
            ton_pubkey_confirmed_notify.notify_waiters();
        } else {
            context
                .deliver_message(
                    user_data_observer.clone(),
                    UnsignedMessage::new(
                        user_data_contract::confirm_ton_account(),
                        user_data_account,
                    ),
                )
                .await
                .context("Failed confirming TON public key")?;
            log::info!("Sent TON public key confirmation");
        }

        if details.eth_address_confirmed {
            eth_address_confirmed_notify.notify_waiters();
        } else {
            let subscriber = context
                .eth_subscribers
                .get_subscriber(bridge_event_configuration.network_configuration.chain_id)
                .ok_or(StakingError::RequiredEthNetworkNotFound)?;
            subscriber
                .verify_relay_staker_address(
                    context.keystore.eth.address(),
                    context.staker_account,
                    &bridge_event_configuration
                        .network_configuration
                        .event_emitter
                        .into(),
                )
                .await
                .context("Failed confirming ETH address")?;
            log::info!("Sent ETH address confirmation")
        }

        log::info!("Waiting confirmation...");
        futures::future::join(ton_notified, eth_notified).await;

        Ok(())
    }
}

impl<'a> StakingContract<'a> {
    fn get_eth_bridge_configuration_details(
        &self,
        shard_accounts: &ShardAccountsMap,
    ) -> Result<EthEventConfigurationDetails> {
        let details = self
            .get_details()
            .context("Failed to get staking details")?;
        let configuration_contract = shard_accounts
            .find_account(&details.bridge_event_config_eth_ton)?
            .context("Bridge ETH event configuration not found")?;

        EthEventConfigurationContract(&configuration_contract)
            .get_details()
            .context("Failed to get ETH bridge configuration details")
    }

    fn get_round_state(&self) -> Result<RoundState> {
        let relay_config = self
            .get_relay_config()
            .context("Failed to get relay config")?;

        let relay_rounds_details = self
            .get_relay_rounds_details()
            .context("Failed to get relay_rounds_details")?;
        log::info!("Relay round details: {:?}", relay_rounds_details);

        let next_elections_account = self
            .get_election_address(relay_rounds_details.current_relay_round + 1)
            .context("Failed to get election address")?;
        log::info!("next_elections_account: {:x}", next_elections_account);

        let elections_state = match relay_rounds_details.current_election_start_time {
            0 if relay_rounds_details.current_election_ended => {
                log::info!("Elections were already finished");
                ElectionsState::Finished
            }
            0 => {
                log::info!("Elections were not started yet");
                ElectionsState::NotStarted {
                    start_time: relay_rounds_details.current_relay_round_start_time
                        + relay_config.time_before_election,
                }
            }
            start_time => {
                log::info!("Elections already started");
                ElectionsState::Started {
                    start_time,
                    end_time: start_time + relay_config.election_time,
                }
            }
        };

        Ok(RoundState {
            elections_state,
            next_elections_account,
        })
    }
}

#[derive(Debug, Clone)]
struct RoundState {
    elections_state: ElectionsState,
    next_elections_account: UInt256,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum ElectionsState {
    NotStarted { start_time: u32 },
    Started { start_time: u32, end_time: u32 },
    Finished,
}

enum PendingElectionsState<'a> {
    NotStarted {
        start_time: u32,
        inner_fut: Notified<'a>,
        outer_fut: Notified<'a>,
    },
    Started {
        end_time: u32,
        inner_fut: Notified<'a>,
        outer_fut: Notified<'a>,
    },
    Finished {
        new_round_fut: Notified<'a>,
    },
}

macro_rules! parse_tokens {
    ($res:expr,$fun:expr, $body:expr, $matched:expr) => {
        match $fun
            .decode_input($body)
            .and_then(|tokens| tokens.unpack().map_err(anyhow::Error::from))
        {
            Ok(parsed) => $res = Some($matched(parsed)),
            Err(e) => {
                log::error!("Failed to parse staking event: {:?}", e);
            }
        }
    };
}

#[derive(Debug)]
enum StakingEvent {
    ElectionStarted(ElectionStartedEvent),
    ElectionEnded(ElectionEndedEvent),
    RelayRoundInitialized(RelayRoundInitializedEvent),
    RelayConfigUpdated(RelayConfigUpdatedEvent),
}

impl ReadFromTransaction for (RoundState, StakingEvent) {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        let start = staking_contract::events::election_started();
        let end = staking_contract::events::election_ended();
        let round_init = staking_contract::events::relay_round_initialized();
        let config_updated = staking_contract::events::relay_config_updated();

        let mut res = None;
        ctx.iterate_events(|id, body| {
            if id == start.id {
                parse_tokens!(res, start, body, StakingEvent::ElectionStarted);
            } else if id == end.id {
                parse_tokens!(res, end, body, StakingEvent::ElectionEnded);
            } else if id == round_init.id {
                parse_tokens!(res, round_init, body, StakingEvent::RelayRoundInitialized);
            } else if id == config_updated.id {
                parse_tokens!(res, config_updated, body, StakingEvent::RelayConfigUpdated);
            }
        });
        let res = res?;

        let contract = match ctx.get_account_state() {
            Ok(contract) => contract,
            Err(e) => {
                log::error!("Failed to find account state after transaction: {:?}", e);
                return None;
            }
        };

        match StakingContract(&contract).get_round_state() {
            Ok(state) => Some((state, res)),
            Err(e) => {
                log::error!("Failed to get round state: {:?}", e);
                None
            }
        }
    }
}

#[derive(Debug)]
enum UserDataEvent {
    RelayMembershipRequested(RelayMembershipRequestedEvent),
    TonPubkeyConfirmed(TonPubkeyConfirmedEvent),
    EthAddressConfirmed(EthAddressConfirmedEvent),
}

impl ReadFromTransaction for UserDataEvent {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        let membership_requested = user_data_contract::events::relay_membership_requested();
        let ton_confirmed = user_data_contract::events::ton_pubkey_confirmed();
        let eth_confirmed = user_data_contract::events::eth_address_confirmed();

        let mut res = None;
        ctx.iterate_events(|id, body| {
            if id == membership_requested.id {
                parse_tokens!(
                    res,
                    membership_requested,
                    body,
                    UserDataEvent::RelayMembershipRequested
                );
            } else if id == ton_confirmed.id {
                parse_tokens!(res, ton_confirmed, body, UserDataEvent::TonPubkeyConfirmed)
            } else if id == eth_confirmed.id {
                parse_tokens!(res, eth_confirmed, body, UserDataEvent::EthAddressConfirmed)
            }
        });
        res
    }
}

#[derive(thiserror::Error, Debug)]
enum StakingError {
    #[error("Required ETH network not found")]
    RequiredEthNetworkNotFound,
    #[error("UserData ETH address mismatch")]
    UserDataEthAddressMismatch,
    #[error("UserData TON public key mismatch")]
    UserDataTonPublicKeyMismatch,
}
