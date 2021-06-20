use std::time::Duration;

use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub address: Url,

    #[serde(with = "relay_utils::serde_time")]
    pub next_block_timeout: Duration,

    #[serde(with = "relay_utils::serde_time")]
    pub fetch_timeout: Duration,

    #[serde(with = "relay_utils::serde_time", default = "default_retry_delay")]
    pub retry_delay: Duration,

    pub parallel_connections: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            address: "https://main.ton.dev/graphql".parse().unwrap(),
            next_block_timeout: Duration::from_secs(60),
            fetch_timeout: Duration::from_secs(10),
            retry_delay: default_retry_delay(),
            parallel_connections: 100,
        }
    }
}

fn default_retry_delay() -> Duration {
    Duration::from_millis(500)
}
