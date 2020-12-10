use std::collections::HashSet;

use crate::db_managment::models::EthTonConfirmationData;
use crate::db_managment::Table;

use super::prelude::*;

#[derive(Clone)]
pub struct EthQueue {
    db: Tree,
}

impl EthQueue {
    pub fn new(db: &Db) -> Result<Self, Error> {
        Ok(Self {
            db: db.open_tree(super::constants::ETH_QUEUE_TREE_NAME)?,
        })
    }
    pub fn get(&self, key: u64) -> Result<Option<HashSet<EthTonConfirmationData>>, Error> {
        match self.db.get(bincode::serialize(&key).unwrap())? {
            Some(data) => Ok(Some(bincode::deserialize(&data)?)),
            None => Ok(None),
        }
    }
    pub fn remove(&self, block: u64) -> Result<(), Error> {
        self.db.remove(bincode::serialize(&block).unwrap())?;
        Ok(())
    }
    pub fn insert(&self, key: &u64, value: &HashSet<EthTonConfirmationData>) -> Result<(), Error> {
        self.db.insert(
            bincode::serialize(&key).unwrap(),
            bincode::serialize(&value).unwrap(),
        )?;
        Ok(())
    }
}
impl Table for EthQueue {
    type Key = u64;
    type Value = HashSet<EthTonConfirmationData>;

    fn dump_elements(&self) -> Vec<(Self::Key, Self::Value)> {
        self.db
            .iter()
            .filter_map(|x| x.ok())
            .map(|(k, v)| {
                (
                    bincode::deserialize(&k).expect("Shouldn't fail"),
                    bincode::deserialize(&v).expect("Shouldn't fail"),
                )
            })
            .collect()
    }
}
