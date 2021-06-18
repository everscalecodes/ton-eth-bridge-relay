use config::Config;
use relay::config::RelayConfig;

#[derive(clap::App)]
struct Opts {}

fn main() {
    let repo = config::Environment::new();
}
