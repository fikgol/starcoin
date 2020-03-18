// Copyright (c) The Starcoin Core Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::storage::Repository;
use anyhow::{bail, format_err, Error, Result};
use logger::prelude::*;
use rocksdb::{ColumnFamilyOptions, DBOptions, Writable, DB};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

pub const DEFAULT_CF_NAME: ColumnFamilyName = "default";
pub const ACCUMULATOR_CF_NAME: ColumnFamilyName = "accumulator";
pub const BLOCK_CF_NAME: ColumnFamilyName = "block";
pub const BLOCK_INFO_CF_NAME: ColumnFamilyName = "block_info";
pub const STATE_NODE_CF_NAME: ColumnFamilyName = "state_node";
pub const TRANSACTION_INFO_CF_NAME: ColumnFamilyName = "transaction_info";

/// Type alias to improve readability.
pub type ColumnFamilyName = &'static str;
/// Type alias to improve readability.
pub type ColumnFamilyOptionsMap = HashMap<ColumnFamilyName, ColumnFamilyOptions>;

pub struct DBStorage {
    db: Arc<DB>,
}

impl DBStorage {
    pub fn new<P: AsRef<Path> + Clone>(db_root_path: P) -> Self {
        Self::open(db_root_path, false, None).expect("Unable to open StarcoinDB")
    }
    pub fn open<P: AsRef<Path> + Clone>(
        db_root_path: P,
        readonly: bool,
        log_dir: Option<P>,
    ) -> Result<Self> {
        let cf_opts_map: ColumnFamilyOptionsMap = [
            (
                /* LedgerInfo CF = */ DEFAULT_CF_NAME,
                ColumnFamilyOptions::default(),
            ),
            (ACCUMULATOR_CF_NAME, ColumnFamilyOptions::default()),
            (BLOCK_CF_NAME, ColumnFamilyOptions::default()),
            (BLOCK_INFO_CF_NAME, ColumnFamilyOptions::default()),
            (STATE_NODE_CF_NAME, ColumnFamilyOptions::default()),
            (TRANSACTION_INFO_CF_NAME, ColumnFamilyOptions::default()),
        ]
        .iter()
        .cloned()
        .collect();
        let path = db_root_path.as_ref().join("starcoindb");

        let db = Arc::new(if readonly {
            let db_log_dir = log_dir
                .ok_or_else(|| format_err!("Must provide log_dir if opening in readonly mode."))?;
            if !db_log_dir.as_ref().is_dir() {
                bail!("Invalid log directory: {:?}", db_log_dir.as_ref());
            }
            info!("log stored at {:?}", db_log_dir.as_ref());
            Self::open_readonly(path.clone(), cf_opts_map, db_log_dir.as_ref().to_path_buf())?
        } else {
            Self::open_inner(path.clone(), cf_opts_map)?
        });

        info!("Opened StarcoinDB at {:?}", path);

        Ok(DBStorage { db })
    }

    fn open_inner<P: AsRef<Path>>(path: P, mut cf_opts_map: ColumnFamilyOptionsMap) -> Result<DB> {
        let mut db_opts = DBOptions::new();
        // For now we set the max total WAL size to be 1G. This config can be useful when column
        // families are updated at non-uniform frequencies.
        db_opts.set_max_total_wal_size(1 << 30);

        // If db exists, just open it with all cfs.
        if Self::db_exists(path.as_ref()) {
            match DB::open_cf(
                db_opts,
                path.as_ref().to_str().ok_or_else(|| {
                    format_err!("Path {:?} can not be converted to string.", path.as_ref())
                })?,
                cf_opts_map.into_iter().collect(),
            ) {
                Ok(db) => return Ok(db),
                Err(e) => bail!("open cf err: {:?}", e),
            }
        }

        // If db doesn't exist, create a db first with all column families.
        db_opts.create_if_missing(true);

        let mut db = match DB::open_cf(
            db_opts.clone(),
            path.as_ref().to_str().ok_or_else(|| {
                format_err!("Path {:?} can not be converted to string.", path.as_ref())
            })?,
            vec![cf_opts_map
                .remove_entry(&DEFAULT_CF_NAME)
                .ok_or_else(|| format_err!("No \"default\" column family name found"))?],
        ) {
            Ok(db) => db,
            Err(e) => bail!("open cf err: {:?}", e),
        };

        cf_opts_map
            .into_iter()
            .map(|(cf_name, cf_opts)| {
                let _cf_handle = db
                    .create_cf((cf_name, cf_opts))
                    .map_err(Self::convert_rocksdb_err)?;
                Ok(())
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(db)
    }

    fn open_readonly<P: AsRef<Path>>(
        path: P,
        cf_opts_map: ColumnFamilyOptionsMap,
        db_log_dir: P,
    ) -> Result<DB> {
        if !Self::db_exists(path.as_ref()) {
            bail!("DB doesn't exists.");
        }

        let mut db_opts = DBOptions::new();

        db_opts.create_if_missing(false);
        db_opts.set_db_log_dir(db_log_dir.as_ref().to_str().ok_or_else(|| {
            format_err!(
                "db_log_dir {:?} can not be converted to string.",
                db_log_dir.as_ref()
            )
        })?);

        Ok(
            match DB::open_cf_for_read_only(
                db_opts,
                path.as_ref().to_str().ok_or_else(|| {
                    format_err!("Path {:?} can not be converted to string.", path.as_ref())
                })?,
                cf_opts_map.into_iter().collect(),
                true,
            ) {
                Ok(db) => db,
                Err(e) => bail!("open cf err: {:?}", e),
            },
        )
    }

    fn db_exists(path: &Path) -> bool {
        let rocksdb_current_file = path.join("CURRENT");
        rocksdb_current_file.is_file()
    }

    pub fn convert_rocksdb_err(msg: String) -> anyhow::Error {
        format_err!("RocksDB internal error: {}.", msg)
    }
}

impl Repository for DBStorage {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        match self.db.clone().get(key).map_err(Self::convert_rocksdb_err) {
            Ok(Some(value)) => Ok(Some(value.to_vec())),
            _ => Ok(None),
        }
    }

    fn put(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
        self.db
            .clone()
            .put(&key, &value)
            .map_err(Self::convert_rocksdb_err)
    }

    fn contains_key(&self, key: Vec<u8>) -> Result<bool> {
        match self.get(&key) {
            Ok(Some(_)) => Ok(true),
            _ => Ok(false),
        }
    }
    fn remove(&self, key: Vec<u8>) -> Result<()> {
        self.db
            .clone()
            .delete(&key)
            .map_err(Self::convert_rocksdb_err)
    }

    fn get_len(&self) -> Result<u64, Error> {
        unimplemented!()
    }

    fn keys(&self) -> Result<Vec<Vec<u8>>, Error> {
        unimplemented!()
    }
}