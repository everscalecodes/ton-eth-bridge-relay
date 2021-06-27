use std::future::Future;
use std::str::FromStr;
use std::time::Duration;

use http::uri::PathAndQuery;
use tryhard::backoff_strategies::BackoffStrategy;
use tryhard::{RetryFutureConfig, RetryPolicy};

pub mod exporter;

pub mod serde_url {
    use serde::de::Error;
    use serde::Deserialize;

    use super::*;

    pub fn serialize<S>(data: &PathAndQuery, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(data.as_str())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<PathAndQuery, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let data = String::deserialize(deserializer)?;
        let data = match data.as_bytes().first() {
            None => "/".to_owned(),
            Some(b'/') => data,
            Some(_) => format!("/{}", data),
        };
        PathAndQuery::from_str(&data).map_err(D::Error::custom)
    }
}

pub mod optional_serde_time {
    use serde::{Deserialize, Serialize};

    use super::*;

    pub fn serialize<S>(data: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        #[serde(transparent)]
        struct Wrapper<'a>(#[serde(with = "serde_time")] &'a Duration);

        match data {
            Some(duration) => serializer.serialize_some(&Wrapper(duration)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(transparent)]
        struct Wrapper(#[serde(with = "serde_time")] Duration);

        Option::<Wrapper>::deserialize(deserializer).map(|wrapper| wrapper.map(|data| data.0))
    }
}

pub mod serde_time {
    use serde::de::Error;
    use serde::Deserialize;

    use super::*;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum DurationValue {
        Number(u64),
        String(String),
    }

    pub fn serialize<S>(data: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_u64(data.as_secs())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value: DurationValue = serde::Deserialize::deserialize(deserializer)?;
        match value {
            DurationValue::Number(seconds) => Ok(Duration::from_secs(seconds)),
            DurationValue::String(string) => {
                let string = string.trim();

                let seconds = if string.chars().all(|c| c.is_digit(10)) {
                    u64::from_str(string).map_err(D::Error::custom)?
                } else {
                    humantime::Duration::from_str(string)
                        .map_err(D::Error::custom)?
                        .as_secs()
                };

                Ok(Duration::from_secs(seconds))
            }
        }
    }
}

/// retries future, logging unsuccessful retries with `message`
pub async fn retry<MakeFutureT, T, E, Fut, BackoffT, OnRetryT>(
    producer: MakeFutureT,
    config: RetryFutureConfig<BackoffT, OnRetryT>,
    message: &'static str,
) -> Result<T, E>
where
    MakeFutureT: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    E: std::error::Error,
    for<'a> BackoffT: BackoffStrategy<'a, E>,
    for<'a> <BackoffT as BackoffStrategy<'a, E>>::Output: Into<RetryPolicy>,
{
    let config = config.on_retry(|attempt, next_delay, error: &E| {
        log::error!(
            "Retrying {} with {} attempt. Next delay: {:?}. Error: {:?}",
            message,
            attempt,
            next_delay,
            error
        );
        std::future::ready(())
    });
    let res = tryhard::retry_fn(producer).with_config(config).await;
    res
}

/// Calculates required number of steps, to get sum of retries ≈ `total_retry_time`.
#[inline]
pub fn calculate_times_from_max_delay(
    start_delay: Duration,
    fraction: f64,
    maximum_delay: Duration,
    total_retry_time: Duration,
) -> u32 {
    let start_delay = start_delay.as_secs_f64();
    let maximum_delay = maximum_delay.as_secs_f64();
    let total_retry_time = total_retry_time.as_secs_f64();
    //calculate number of steps to saturate. E.G. If maximum timeout is 600, then you'll have 9 steps, before reaching it.
    let saturation_steps =
        (f64::log10((maximum_delay - start_delay) / start_delay) / f64::log10(fraction)).floor();
    let time_to_saturate =
        start_delay * (1f64 - fraction.powf(saturation_steps)) / (1f64 - fraction);
    let remaining_time = total_retry_time - time_to_saturate;
    let steps = remaining_time / maximum_delay;
    (steps + saturation_steps).ceil() as u32
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;
    #[test]
    fn test_delay_times() {
        let res = super::calculate_times_from_max_delay(
            Duration::from_secs(1),
            2f64,
            Duration::from_secs(600),
            Duration::from_secs(86400),
        );
        assert_eq!(153, res);
    }

    #[derive(Deserialize)]
    struct TestStruct {
        #[serde(with = "serde_time")]
        interval: Duration,
    }

    #[test]
    fn test_deserialize() {
        let string = r#"interval: 5s"#;
        let object: TestStruct = serde_yaml::from_str(&string).unwrap();
        assert_eq!(object.interval.as_secs(), 5);

        let string = r#"interval: 1m 30s"#;
        let object: TestStruct = serde_yaml::from_str(&string).unwrap();
        assert_eq!(object.interval.as_secs(), 90);

        let string = r#"interval: 123"#;
        let object: TestStruct = serde_yaml::from_str(&string).unwrap();
        assert_eq!(object.interval.as_secs(), 123);
    }

    #[derive(Deserialize)]
    struct OptionalTestStruct {
        test: u32,
        #[serde(default, with = "optional_serde_time")]
        interval: Option<Duration>,
    }

    #[test]
    fn test_deserialize_optional() {
        let string = r#"---
test: 123"#;
        let object: OptionalTestStruct = serde_yaml::from_str(&string).unwrap();
        assert!(object.interval.is_none());

        let string = r#"---
test: 123 
interval:"#;
        let object: OptionalTestStruct = serde_yaml::from_str(&string).unwrap();
        assert!(object.interval.is_none());

        let string = r#"---
test: 123
interval: 1m 30s"#;
        let object: OptionalTestStruct = serde_yaml::from_str(&string).unwrap();
        assert_eq!(object.interval, Some(Duration::from_secs(90)));
    }
}
