use anyhow::anyhow;
use anyhow::Error;
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Input, Password, Select};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use structopt::StructOpt;

fn parse_url(url: &str) -> Result<Url, Error> {
    Ok(Url::parse(url)?)
}

#[derive(StructOpt)]
struct Arguments {
    #[structopt(short, long, parse(try_from_str = parse_url))]
    server_addr: Url,
}

#[derive(Serialize, Debug)]
struct InitData {
    ton_seed: Vec<String>,
    eth_seed: Vec<String>,
    password: String,
}

fn provide_ton_seed() -> Result<Vec<String>, Error> {
    let input: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Provide ton seed words. 12 words are needed.")
        .interact_text()?;
    let words: Vec<String> = input.split(" ").map(|x| x.to_string()).collect();
    if words.len() != 12 {
        return Err(anyhow!("{} words for ton seed are provided", words.len()));
    }
    Ok(words)
}

fn provide_eth_seed() -> Result<Vec<String>, Error> {
    let input: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Provide ton seed words.")
        .interact_text()?;
    let words: Vec<String> = input.split(" ").map(|x| x.to_string()).collect();
    if words.len() < 12 {
        return Err(anyhow!(
            "{} words for eth seed are provided which is not enough for high entropy",
            words.len()
        ));
    }
    Ok(words)
}

fn provide_password() -> Result<String, Error> {
    let password = Password::with_theme(&ColorfulTheme::default())
        .with_prompt("Password, longer then 8 symbols")
        .with_confirmation("Repeat password", "Error: the passwords don't match.")
        .interact()?;
    if password.len() < 8 {
        return Err(anyhow!("Password len is {}", password.len()));
    }
    Ok(password)
}

fn unlock_node() -> Result<String, Error> {
    let password = Password::with_theme(&ColorfulTheme::default())
        .with_prompt("Password:")
        .interact()?;
    Ok(password)
}

fn init() -> Result<InitData, Error> {
    let ton_seed = provide_ton_seed()?;
    let eth_seed = provide_eth_seed()?;
    let password = provide_password()?;
    Ok(InitData {
        password,
        eth_seed,
        ton_seed,
    })
}

fn main() -> Result<(), Error> {
    let args: Arguments = Arguments::from_args();
    const ACTIONS: &[&str; 2] = &["Init", "Provide password"];
    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("What do you want?")
        .default(0)
        .items(&ACTIONS[..])
        .interact()
        .unwrap();
    if selection == 0 {
        let init_data = init()?;
        let client = reqwest::blocking::Client::new();
        let url = args.server_addr.join("init")?;
        let response = client.post(url).json(&init_data).send()?;
        if response.status().is_success() {
            println!("Initialized successfully");
        }
    } else if selection == 1 {
        let password = unlock_node()?;
    }
    Ok(())
}
