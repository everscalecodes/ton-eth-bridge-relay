use anyhow::Result;
use nekoton_abi::*;
use ton_types::UInt256;

pub use self::models::*;
use crate::utils::*;

pub mod base_event_configuration_contract;
pub mod bridge_contract;
pub mod connector_contract;
pub mod eth_event_configuration_contract;
pub mod eth_event_contract;
pub mod ton_event_configuration_contract;
pub mod ton_event_contract;

mod models;

pub struct EthEventContract<'a>(pub &'a ExistingContract);

impl EthEventContract<'_> {
    pub fn get_details(&self) -> Result<TonEventDetails> {
        let function = eth_event_contract::get_details();
        let result = self.0.run_local(function, &[answer_id()])?.unpack()?;
        Ok(result)
    }
}

pub struct TonEventContract<'a>(pub &'a ExistingContract);

impl TonEventContract<'_> {
    pub fn get_details(&self) -> Result<TonEventDetails> {
        let function = ton_event_contract::get_details();
        let result = self.0.run_local(function, &[answer_id()])?.unpack()?;
        Ok(result)
    }
}

pub struct EventConfigurationBaseContract<'a>(pub &'a ExistingContract);

impl EventConfigurationBaseContract<'_> {
    pub fn get_type(&self) -> Result<EventType> {
        let function = base_event_configuration_contract::get_type();
        let event_type = self.0.run_local(function, &[answer_id()])?.unpack_first()?;
        Ok(event_type)
    }
}

pub struct EthEventConfigurationContract<'a>(pub &'a ExistingContract);

impl EthEventConfigurationContract<'_> {
    pub fn get_details(&self) -> Result<EthEventConfigurationDetails> {
        let function = eth_event_configuration_contract::get_details();
        let details = self.0.run_local(function, &[answer_id()])?.unpack()?;
        Ok(details)
    }
}

pub struct TonEventConfigurationContract<'a>(pub &'a ExistingContract);

impl TonEventConfigurationContract<'_> {
    pub fn get_details(&self) -> Result<TonEventConfigurationDetails> {
        let function = ton_event_configuration_contract::get_details();
        let details = self.0.run_local(function, &[answer_id()])?.unpack()?;
        Ok(details)
    }
}

pub struct BridgeContract<'a>(pub &'a ExistingContract);

impl BridgeContract<'_> {
    pub fn derive_connector_address(&self, id: u64) -> Result<UInt256> {
        let function = bridge_contract::derive_connector_address();
        let input = [id.token_value().named("id")];
        let address: ton_block::MsgAddrStd = self.0.run_local(function, &input)?.unpack_first()?;

        Ok(UInt256::from_be_bytes(&address.address.get_bytestring(0)))
    }
}

pub struct ConnectorContract<'a>(pub &'a ExistingContract);

impl ConnectorContract<'_> {
    pub fn get_details(&self) -> Result<ConnectorDetails> {
        let function = connector_contract::get_details();
        let details = self.0.run_local(function, &[])?.unpack()?;
        Ok(details)
    }
}
