use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Error, Result};
use bip39::Language;
use clap::Clap;
use config::Config;
use secstr::SecUtf8;
use serde::{Deserialize, Serialize};
use tap::Pipe;
use url::Url;

use relay::config::TonTransportConfig;
use relay::crypto::key_managment::KeyData;
use relay::crypto::recovery::{derive_from_words_eth, derive_from_words_ton};

// #[cfg(feature = "graphql-transport")]
// use relay_ton::transport::graphql_transport::Config as TonGraphQLConfig;
// #[cfg(feature = "graphql-transport")]
// use relay_ton::transport::tonlib_transport::default_mainnet_config;
// #[cfg(feature = "tonlib-transport")]
// use relay_ton::transport::tonlib_transport::Config as TonTonlibConfig;

#[derive(Clap, Debug)]
struct Opts {
    #[clap(subcommand)]
    actions: Subcommand,
}

#[derive(Clap, Debug)]
enum Subcommand {
    Restore(Restore),
    Init(Init),
    Backup(Backup),
    GenKeys(GenKeys),
    ImportKeys(ImportKeys),
}

#[derive(Clap, Debug)]
struct GenKeys {
    /// Path to relay keys
    #[clap(default_value = "./relay-keys.json")]
    #[clap(long, short)]
    crypto_keys_path: PathBuf,
    #[clap(long)]
    pub ton_derivation_path: Option<String>,
    #[clap(long)]
    pub eth_derivation_path: Option<String>,
}

#[derive(Clap, Debug)]
struct ImportKeys {
    #[clap(long)]
    pub ton_derivation_path: Option<String>,
    #[clap(long)]
    pub eth_derivation_path: Option<String>,
    #[clap(long)]
    pub ton_seed: Option<String>,
    #[clap(long)]
    pub eth_seed: Option<String>,
}

#[derive(Clap, Debug)]
struct Backup {
    #[clap(default_value = "./relay-keys.json")]
    #[clap(long, short)]
    crypto_keys_path: PathBuf,
}

#[derive(Clap, Debug, Clone)]
struct Init {
    /// Path to relay keys
    #[clap(default_value = "./relay-keys.json")]
    #[clap(long, short)]
    crypto_keys_path: PathBuf,
    ///Path to base relay config
    #[clap(default_value = "relay-config.yaml")]
    #[clap(long, short)]
    relay_config_path: PathBuf,
    #[clap(long)]
    pub ton_seed: Option<String>,
    #[clap(long)]
    pub eth_seed: Option<String>,
    #[clap(long)]
    pub ton_derivation_path: Option<String>,
    #[clap(long)]
    pub eth_derivation_path: Option<String>,
    #[clap(long)]
    /// Url of eth node
    pub eth_node_address: Url,
    #[clap(long)]
    //todo set default address after stabilization
    pub ton_bridge_contract_address: ton_block::MsgAddressInt,
    #[clap(long)]
    // #[clap(group = "transport")]
    #[clap(required_unless_present = "adnl-endpoint-address")]
    pub graphql_endpoint_address: Option<Url>,
    #[clap(long)]
    // #[clap(group = "transport", required(true))]
    #[clap(required_unless_present = "graphql-endpoint-address")]
    #[cfg(feature = "tonlib-transport")]
    #[clap(requires = "adnl-server-key")]
    pub adnl_endpoint_address: Option<SocketAddr>,
    #[clap(long)]
    pub adnl_server_key: Option<String>,
    #[clap(long)]
    pub eth_bridge_address: relay_eth::Address,
    #[clap(long)]
    pub staking_account: relay_eth::Address,
}

#[derive(Clap, Debug)]
struct Restore {
    /// Path to relay keys
    #[clap(default_value = "./relay-keys.json")]
    #[clap(long, short)]
    crypto_keys_path: PathBuf,
    #[clap(long)]
    pub ton_seed: String,
    #[clap(long)]
    pub eth_seed: String,
    #[clap(long)]
    pub ton_derivation_path: Option<String>,
    #[clap(long)]
    pub eth_derivation_path: Option<String>,
}

fn main() -> Result<()> {
    let options = Opts::parse();
    dbg!(&options);
    match options.actions {
        Subcommand::Init(_) => Ok(()),
        Subcommand::Restore(a) => a.run(),
        Subcommand::Backup(a) => Ok(()),
        Subcommand::GenKeys(a) => a.run(),
        Subcommand::ImportKeys(_) => Ok(()),
    }?;
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename = "SCREAMING_SNAKE_CASE")]
pub struct Password {
    pub password: SecUtf8,
}

impl Password {
    fn get() -> Result<SecUtf8> {
        let mut repo = Config::default();
        let env = config::Environment::new();
        repo.merge(env)?;
        let password = repo
            .try_into::<Password>()
            .map_err(|e| Error::new(e).context("Failed initializing config: "))?
            .pipe(|x| x.password);
        Ok(password)
    }
}
//
// fn init(init_data: Init) -> Result<()> {
//     // use relay_models::models::InitData;
//
//     let mut repo = Config::default();
//     let env = config::Environment::new();
//     repo.merge(env)?;
//     let password = repo
//         .try_into::<Password>()
//         .map_err(|e| Error::new(e).context("Failed initializing config: "))?
//         .pipe(|x| x.password);
//
//     let parsed_data = parse_init_data(init_data.clone())?;
//     dbg!(&parsed_data);
//
//     let eth_private_key = derive_from_words_eth(
//         Language::English,
//         &parsed_data.eth_seed.unsecure(),
//         init_data.eth_derivation_path.as_deref(),
//     )
//     .map_err(|e| e.context("Failed deriving eth private key from seed:"))?;
//
//     let ton_key_pair = derive_from_words_ton(
//         Language::English,
//         &parsed_data.ton_seed.unsecure(),
//         init_data.ton_derivation_path.as_deref(),
//     )
//     .map_err(|e| e.context("Failed deriving ton private key from seed:"))?;
//
//     KeyData::init(
//         &init_data.crypto_keys_path,
//         password,
//         eth_private_key,
//         ton_key_pair,
//     )
//     .map_err(|e| e.context("Failed saving init data:"))?;
//
//     let mut relay_config = relay::config::RelayConfig::default();
//     relay_config.eth_settings.bridge_address = init_data.eth_bridge_address;
//     relay_config.eth_settings.node_address = init_data.eth_node_address;
//     relay_config.keys_path = init_data.crypto_keys_path;
//     relay_config.ton_settings.bridge_contract_address = init_data
//         .ton_bridge_contract_address
//         .pipe(|x| x.to_string())
//         .pipe(relay::config::TonAddress);
//     // relay_config.ton_settings.transport = parsed_data.network_config;
//     Ok(())
// }

#[derive(Debug)]
struct ParsedInitData {
    // pub network_config: TonTransportConfig,
    eth_seed: SecUtf8,
    ton_seed: SecUtf8,
}

fn generate_entropy<const N: usize>() -> Result<[u8; N], Error> {
    use ring::rand::SecureRandom;

    let rng = ring::rand::SystemRandom::new();

    let mut entropy = [0; N];
    rng.fill(&mut entropy).map_err(|e| Error::msg(e))?;
    Ok(entropy)
}

fn generate_words<const N: usize>(entropy: [u8; 16]) -> Result<SecUtf8> {
    let mnemonic = bip39::Mnemonic::from_entropy(&entropy, Language::English)
        .map_err(|e| Error::msg(e).context("Failed generating mnemonic"))?
        .into_phrase();
    Ok(SecUtf8::from(mnemonic))
}

// fn parse_init_data(init_data: Init) -> Result<ParsedInitData> {
//     let ton_seed = match init_data.ton_seed {
//         None => generate_words(generate_entropy()?)?,
//         Some(a) => a.into(),
//     };
//     let eth_seed = match init_data.eth_seed {
//         None => generate_words(generate_entropy()?)?,
//         Some(a) => a.into(),
//     };
//
//     if !((init_data.adnl_endpoint_address.is_some() && init_data.adnl_server_key.is_some())
//         || (init_data.graphql_endpoint_address.is_some()))
//     {
//         anyhow::bail!("ADNL_ENDPOINT_ADDRESS and ADNL_SERVER_KEY or GRAPHQL_ENDPOINT_ADDRESS must be provided")
//     }
//
//     // let network_config = match init_data.graphql_endpoint_address {
//     //     None => {
//     //         #[cfg(feature = "tonlib-transport")]
//     //         {
//     //             let adnl_endpoint_address: SocketAddr = init_data.adnl_endpoint_address.unwrap();
//     //             let andl_pubkey = init_data.adnl_server_key.unwrap(); //todo add validation
//     //             let mut config = default_mainnet_config();
//     //             config.server_address = adnl_endpoint_address;
//     //             config.server_key = andl_pubkey;
//     //             TonTransportConfig::Tonlib(config)
//     //
//     //     }
//     //     Some(endpoint) => {
//     //         let mut config = TonGraphQLConfig::default();
//     //         config.address = endpoint;
//     //     }
//     // };
//
//     Ok(ParsedInitData {
//         // network_config,
//         eth_seed,
//         ton_seed,
//     })
// }

impl GenKeys {
    fn run(self) -> Result<()> {
        let password = Password::get()?;
        let GenKeys {
            crypto_keys_path,
            ton_derivation_path,
            eth_derivation_path,
        } = self;
        let eth_words = generate_words::<16>(
            generate_entropy().map_err(|e| e.context("Failed gnerating entropy"))?,
        )
        .map_err(|e| e.context("Failed generating eth words"))?;
        let ton_words = generate_words::<16>(
            generate_entropy().map_err(|e| e.context("Failed gnerating entropy"))?,
        )
        .map_err(|e| e.context("Failed generating eth words"))?;
        println!("ETH WORDS: {}", eth_words.unsecure());
        println!("TON WORDS: {}", ton_words.unsecure());
        restore_from_mnemonics(
            password,
            &crypto_keys_path,
            ton_derivation_path,
            eth_derivation_path,
            eth_words,
            ton_words,
        )
    }
}

fn restore_from_mnemonics(
    password: SecUtf8,
    crypto_keys_path: &PathBuf,
    ton_derivation_path: Option<String>,
    eth_derivation_path: Option<String>,
    eth_words: SecUtf8,
    ton_words: SecUtf8,
) -> Result<()> {
    let eth_private_key = derive_from_words_eth(
        Language::English,
        &eth_words.unsecure(),
        eth_derivation_path.as_deref(),
    )
    .map_err(|e| e.context("Failed deriving eth private key from seed:"))?;

    let ton_key_pair = derive_from_words_ton(
        Language::English,
        &ton_words.unsecure(),
        ton_derivation_path.as_deref(),
    )
    .map_err(|e| e.context("Failed deriving ton private key from seed:"))?;
    KeyData::init(&crypto_keys_path, password, eth_private_key, ton_key_pair)
        .map_err(|e| e.context("Failed saving config"))?;
    println!("Keys saved to {}", crypto_keys_path.display());
    Ok(())
}

impl Restore {
    fn run(self) -> Result<()> {
        let Restore {
            crypto_keys_path,
            ton_seed,
            eth_seed,
            ton_derivation_path,
            eth_derivation_path,
        } = self;
        let password = Password::get().map_err(|e| e.context("Failed getting password"))?;
        restore_from_mnemonics(
            password,
            &crypto_keys_path,
            ton_derivation_path,
            eth_derivation_path,
            eth_seed.into(),
            ton_seed.into(),
        )
    }
}
