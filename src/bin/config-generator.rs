use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Error, Result};
use bip39::Language;
use clap::Clap;
use config::Config;
use secstr::SecUtf8;
use serde::{Deserialize, Serialize};
use tap::Pipe;
use ton_block::MsgAddressInt;
use url::Url;

use relay::crypto::recovery::{derive_from_words_eth, derive_from_words_ton};
use relay::prelude::FromStr;

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
}

#[derive(Clap, Debug)]
struct Backup {}

#[derive(Clap, Debug)]
struct Init {
    #[clap(default_value = "./relay-keys.json")]
    #[clap(long, short)]
    generated_config_path: PathBuf,
}

#[derive(Clap, Debug)]
struct Restore {}

fn main() -> Result<()> {
    let options = Opts::parse();
    dbg!(&options);
    match options.actions {
        Subcommand::Init(a) => init(a),
        Subcommand::Restore(_) => Ok(()),
        Subcommand::Backup(_) => Ok(()),
    }?;
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename = "SCREAMING_SNAKE_CASE")]
pub struct InitData {
    pub ton_seed: Option<SecUtf8>,
    pub eth_seed: Option<SecUtf8>,
    pub password: SecUtf8,
    pub ton_derivation_path: Option<String>,
    pub eth_derivation_path: Option<String>,
    pub eth_node_address: String,
    pub bridge_contract_address: String, //todo set default address after stabilization
    pub graphql_endpoint_address: Option<String>,
    pub adnl_endpoint_address: Option<String>,
    pub adnl_server_key: Option<String>,
    pub eth_relay_address: String,
}

fn init(init_data: Init) -> Result<()> {
    // use relay_models::models::InitData;

    let mut repo = Config::default();
    let env = config::Environment::new();
    repo.merge(env)?;
    let config: InitData = repo
        .try_into()
        .map_err(|e| Error::new(e).context("Failed initializing config: "))?;
    let parsed_data = parse_init_data(config)?;
    dbg!(&parsed_data);
    let eth_private_key =
        derive_from_words_eth(Language::English, &parsed_data.eth_seed.unsecure(), None)
            .map_err(|e| e.context("Failed deriving eth private key from seed:"))?;

    let ton_key_pair =
        derive_from_words_ton(Language::English, &parsed_data.ton_seed.unsecure(), None)
            .map_err(|e| e.context("Failed deriving ton private key from seed:"))?;
    relay::crypto::key_managment::KeyData::init(
        init_data.generated_config_path,
        parsed_data.password.into(),
        eth_private_key,
        ton_key_pair,
    )
    .map_err(|e| e.context("Failed saving init data:"))?;
    Ok(())
}

#[derive(Debug)]
enum NetworkingConfig {
    Adnl {
        adnl_endpoint_address: SocketAddr,
        andl_pubkey: String,
    },
    Gql {
        endpoint: Url,
    },
}

#[derive(Debug)]
struct ParsedInitData {
    pub network_config: NetworkingConfig,
    password: SecUtf8,

    staking_account: relay_eth::Address,
    bridge_contract_address: MsgAddressInt,
    eth_node_address: Url,
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

fn generate_words(entropy: [u8; 16]) -> Result<SecUtf8> {
    let mnemonic = bip39::Mnemonic::from_entropy(&entropy, Language::English)
        .map_err(|e| Error::msg(e).context("Failed generating mnemonic"))?
        .into_phrase();
    Ok(SecUtf8::from(mnemonic))
}

fn parse_init_data(data: InitData) -> Result<ParsedInitData> {
    let ton_seed = match data.ton_seed {
        None => generate_words(generate_entropy()?)?,
        Some(a) => a,
    };
    let eth_seed = match data.eth_seed {
        None => generate_words(generate_entropy()?)?,
        Some(a) => a,
    };
    let eth_node_address: url::Url = data
        .eth_node_address
        .parse()
        .map_err(|e| Error::new(e).context("Failed parsing eth node address as url"))?;
    if !((data.adnl_endpoint_address.is_some() && data.adnl_server_key.is_some())
        || (data.graphql_endpoint_address.is_some()))
    {
        anyhow::bail!("ADNL_ENDPOINT_ADDRESS and ADNL_SERVER_KEY or GRAPHQL_ENDPOINT_ADDRESS must be provided")
    }

    let network_config = match data.graphql_endpoint_address {
        None => {
            let adnl_endpoint_address: SocketAddr = data
                .adnl_endpoint_address
                .unwrap()
                .parse()
                .map_err(|e| Error::new(e).context("Failed parsing adnl endpoint address:"))?;
            let andl_pubkey = data.adnl_server_key.unwrap(); //todo add validation
            NetworkingConfig::Adnl {
                adnl_endpoint_address,
                andl_pubkey,
            }
        }
        Some(a) => NetworkingConfig::Gql {
            endpoint: a
                .parse()
                .map_err(|e| Error::new(e).context("Bad gql endpoint address:"))?,
        },
    };
    let bridge_contract_address = ton_block::MsgAddressInt::from_str(&data.bridge_contract_address)
        .map_err(|e| Error::msg(e).context("Failed parsing bridge contract address"))?;

    let staking_account = data
        .eth_relay_address
        .pipe(|x| relay_eth::Address::from_str(&x))
        .map_err(|e| Error::new(e).context("Failed parsing ethereum relay address"))?;

    Ok(ParsedInitData {
        network_config,
        password: data.password,
        staking_account,
        bridge_contract_address,
        eth_node_address,
        eth_seed,
        ton_seed,
    })
}
