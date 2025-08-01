/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use path_hash::PathHashBytes;
use path_hash::PathWithHash;
use sql_ext::Connection;
use stats::prelude::*;
use vec1::Vec1;

define_stats! {
    prefix = "mononoke.filenodes";
    filenodes_conn_checkout: timeseries(Rate, Sum),
    history_conn_checkout: timeseries(Rate, Sum),
    paths_conn_checkout: timeseries(Rate, Sum),
}

#[derive(Copy, Clone)]
pub enum AcquireReason {
    Filenodes,
    History,
    Paths,
}

#[derive(Hash, Eq, PartialEq)]
pub struct ShardId {
    id: usize,
}

pub struct Connections {
    connections: Vec1<Connection>,
}

impl Connections {
    pub fn new(connections: Vec1<Connection>) -> Self {
        Self { connections }
    }
}

impl Connections {
    pub fn shard_id(&self, ph: &PathHashBytes) -> ShardId {
        ShardId {
            id: PathWithHash::shard_number_by_hash(ph, self.connections.len()),
        }
    }

    pub fn checkout<'a>(&'a self, pwh: &PathWithHash<'_>, reason: AcquireReason) -> &'a Connection {
        let shard_id = self.shard_id(&pwh.hash);
        self.checkout_by_shard_id(shard_id, reason)
    }

    pub fn checkout_by_shard_id<'a>(
        &'a self,
        shard_id: ShardId,
        reason: AcquireReason,
    ) -> &'a Connection {
        match reason {
            AcquireReason::Filenodes => STATS::filenodes_conn_checkout.add_value(1),
            AcquireReason::History => STATS::history_conn_checkout.add_value(1),
            AcquireReason::Paths => STATS::paths_conn_checkout.add_value(1),
        };

        &self.connections[shard_id.id] as _
    }
}
