use std::path::{Path, PathBuf};

use anyhow::{Error, Result};
use bip39::Language;
use clap::Clap;
use config::Config;
use relay::crypto::key_managment::KeyData;
use relay::crypto::recovery::{derive_from_words_eth, derive_from_words_ton};
use secstr::SecUtf8;
use serde::{Deserialize, Serialize};
use tap::Pipe;

#[derive(Clap, Debug)]
struct Opts {
    #[clap(subcommand)]
    actions: Subcommand,
}

#[derive(Clap, Debug)]
enum Subcommand {
    Restore(Restore),
    Backup(Backup),
    GenKeys(GenKeys),
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
        Subcommand::Restore(a) => a.run(),
        Subcommand::Backup(a) => a.run(),
        Subcommand::GenKeys(a) => a.run(),
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
            .map_err(|e| Error::new(e).context("Failed getting password from env"))?
            .pipe(|x| x.password);
        Ok(password)
    }
}

fn generate_entropy<const N: usize>() -> Result<[u8; N], Error> {
    use ring::rand::SecureRandom;

    let rng = ring::rand::SystemRandom::new();

    let mut entropy = [0; N];
    rng.fill(&mut entropy).map_err(Error::msg)?;
    Ok(entropy)
}

fn generate_words<const N: usize>(entropy: [u8; 16]) -> Result<SecUtf8> {
    let mnemonic = bip39::Mnemonic::from_entropy(&entropy, Language::English)
        .map_err(|e| Error::msg(e).context("Failed generating mnemonic"))?
        .into_phrase();
    Ok(SecUtf8::from(mnemonic))
}

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
    crypto_keys_path: &Path,
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
    KeyData::init(
        &crypto_keys_path,
        password,
        eth_private_key,
        ton_key_pair,
        ton_words,
        eth_words,
    )
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

impl Backup {
    fn run(self) -> Result<()> {
        let path = self.crypto_keys_path;
        let data = std::fs::read_to_string(path)
            .map_err(|e| Error::new(e).context("Failed reading config: "))?;
        let password = Password::get()?;
        let config: relay::crypto::key_managment::CryptoData = serde_json::from_str(&data)
            .map_err(|e| Error::new(e).context("Failed parsing config: "))?;
        let (ton, eth) = config.recover(password)?;
        println!("TON: {}", ton.unsecure());
        println!("ETH: {}", eth.unsecure());
        Ok(())
    }
}
