use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use http::uri::PathAndQuery;
use nekoton_utils::*;
use rand::Rng;
use secstr::SecUtf8;
use serde::{Deserialize, Serialize};

pub use self::eth_config::*;
pub use self::stored_keys::*;
pub use self::verification_state::*;
use crate::utils::*;

mod eth_config;
mod stored_keys;
mod verification_state;

/// Main application config (full). Used to run relay
#[derive(Serialize, Deserialize)]
pub struct AppConfig {
    /// Password, used to encode and decode data in keystore
    pub master_password: SecUtf8,

    /// Whether the protected area will be used for the keystore
    #[serde(default)]
    pub require_protected_keystore: bool,

    /// Staker address from which keys were submitted
    #[serde(with = "serde_address")]
    pub staker_address: ton_block::MsgAddressInt,

    /// Bridge related settings
    pub bridge_settings: BridgeConfig,

    /// TON node settings
    #[serde(default)]
    pub node_settings: NodeConfig,

    /// Prometheus metrics exporter settings.
    /// Completely disable when not specified
    #[serde(default)]
    pub metrics_settings: Option<MetricsConfig>,

    /// log4rs settings.
    /// See [docs](https://docs.rs/log4rs/1.0.0/log4rs/) for more details
    #[serde(default = "default_logger_settings")]
    pub logger_settings: serde_yaml::Value,
}

/// Main application config (brief). Used for simple commands that require only password
#[derive(Serialize, Deserialize)]
pub struct BriefAppConfig {
    /// Password, used to encode and decode data in keystore
    #[serde(default)]
    pub master_password: Option<SecUtf8>,
}

/// Bridge related settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BridgeConfig {
    /// Path to the file with keystore data
    pub keys_path: PathBuf,

    /// Bridge contract address
    #[serde(with = "serde_address")]
    pub bridge_address: ton_block::MsgAddressInt,

    /// If set, relay will not participate in elections. Default: false
    #[serde(default)]
    pub ignore_elections: bool,

    /// EVM networks settings
    pub networks: Vec<EthConfig>,

    /// ETH address verification settings
    #[serde(default)]
    pub address_verification: AddressVerificationConfig,
}

/// ETH address verification settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AddressVerificationConfig {
    /// Minimal balance on user's wallet to start address verification
    /// Default: 50000000 (0.05 ETH)
    pub min_balance_gwei: u64,

    /// Fixed gas price. Default: 300
    pub gas_price_gwei: u64,

    /// Path to the file with transaction state.
    /// Default: `./verification-state.json`
    pub state_path: PathBuf,
}

impl Default for AddressVerificationConfig {
    fn default() -> Self {
        Self {
            min_balance_gwei: 50000000,
            gas_price_gwei: 300,
            state_path: "verification-state.json".into(),
        }
    }
}

/// TON node settings
#[derive(Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NodeConfig {
    /// Node public ip. Automatically determines if None
    pub adnl_public_ip: Option<Ipv4Addr>,

    /// Node port. Default: 30303
    pub adnl_port: u16,

    /// Path to the DB directory. Default: `./db`
    pub db_path: PathBuf,

    /// Path to the ADNL keys. Default: `./adnl-keys.json`.
    /// NOTE: generates new keys if specified path doesn't exist
    pub temp_keys_path: PathBuf,

    /// Allowed DB size in bytes. Default: one third of all machine RAM
    pub max_db_memory_usage: usize,

    /// Archives map queue. Default: 16
    pub parallel_archive_downloads: u32,

    /// Whether old shard states will be removed every 10 minutes
    pub states_gc_enabled: bool,

    /// Whether old blocks will be removed on each new key block
    pub blocks_gc_enabled: bool,
}

impl NodeConfig {
    pub async fn build_indexer_config(self) -> Result<ton_indexer::NodeConfig> {
        // Determine public ip
        let ip_address = match self.adnl_public_ip {
            Some(address) => address,
            None => public_ip::addr_v4()
                .await
                .ok_or(ConfigError::PublicIpNotFound)?,
        };
        log::info!("Using public ip: {}", ip_address);

        // Generate temp keys
        let adnl_keys = ton_indexer::NodeKeys::load(self.temp_keys_path, false)
            .context("Failed to load temp keys")?;

        // Prepare DB folder
        std::fs::create_dir_all(&self.db_path)?;

        // Done
        Ok(ton_indexer::NodeConfig {
            ip_address: SocketAddrV4::new(ip_address, self.adnl_port),
            adnl_keys,
            rocks_db_path: self.db_path.join("rocksdb"),
            file_db_path: self.db_path.join("files"),
            state_gc_options: self.states_gc_enabled.then(|| ton_indexer::StateGcOptions {
                offset_sec: rand::thread_rng().gen_range(0..3600),
                interval_sec: 3600,
            }),
            blocks_gc_options: self
                .blocks_gc_enabled
                .then(|| ton_indexer::BlocksGcOptions {
                    kind: ton_indexer::BlocksGcKind::BeforePreviousKeyBlock,
                    enable_for_sync: true,
                    ..Default::default()
                }),
            shard_state_cache_options: None,
            archives_enabled: false,
            old_blocks_policy: Default::default(),
            max_db_memory_usage: self.max_db_memory_usage,
            parallel_archive_downloads: self.parallel_archive_downloads,
            adnl_options: Default::default(),
            rldp_options: Default::default(),
            dht_options: Default::default(),
            neighbours_options: Default::default(),
            overlay_shard_options: Default::default(),
        })
    }
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            adnl_public_ip: None,
            adnl_port: 30303,
            db_path: "db".into(),
            temp_keys_path: "adnl-keys.json".into(),
            max_db_memory_usage: ton_indexer::default_max_db_memory_usage(),
            parallel_archive_downloads: 16,
            states_gc_enabled: true,
            blocks_gc_enabled: true,
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(default)]
pub struct MetricsConfig {
    /// Listen address of metrics. Used by the client to gather prometheus metrics.
    /// Default: `127.0.0.1:10000`
    pub listen_address: SocketAddr,

    /// Path to the metrics.
    /// Default: `/`
    #[serde(with = "serde_url")]
    pub metrics_path: PathAndQuery,

    /// Metrics update interval in seconds. Default: 10
    pub collection_interval_sec: u64,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            listen_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 10000),
            metrics_path: PathAndQuery::from_static("/"),
            collection_interval_sec: 10,
        }
    }
}

impl ConfigExt for ton_indexer::GlobalConfig {
    fn from_file<P>(path: &P) -> Result<Self>
    where
        P: AsRef<Path>,
    {
        let file = std::fs::File::open(path)?;
        let reader = std::io::BufReader::new(file);
        let config = serde_json::from_reader(reader)?;
        Ok(config)
    }
}

pub trait ConfigExt: Sized {
    fn from_file<P>(path: &P) -> Result<Self>
    where
        P: AsRef<Path>;
}

fn default_logger_settings() -> serde_yaml::Value {
    const DEFAULT_LOG4RS_SETTINGS: &str = r##"
    appenders:
      stdout:
        kind: console
        encoder:
          pattern: "{d(%Y-%m-%d %H:%M:%S %Z)(utc)} - {h({l})} {M} = {m} {n}"
    root:
      level: error
      appenders:
        - stdout
    loggers:
      relay:
        level: info
        appenders:
          - stdout
        additive: false
    "##;
    serde_yaml::from_str(DEFAULT_LOG4RS_SETTINGS).unwrap()
}

#[derive(thiserror::Error, Debug)]
enum ConfigError {
    #[error("Failed to find public ip")]
    PublicIpNotFound,
}
