/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use async_runtime::block_on;
pub use edenapi_trait::Response;
pub use edenapi_trait::ResponseMeta;
use futures::prelude::*;
use http_client::Stats;

use crate::errors::SaplingRemoteApiError;

/// Non-async version of `Response`.
pub struct BlockingResponse<T> {
    pub entries: Vec<T>,
    pub stats: Stats,
}

impl<T> BlockingResponse<T> {
    pub fn from_async<F>(fetch: F) -> Result<Self, SaplingRemoteApiError>
    where
        F: Future<Output = Result<Response<T>, SaplingRemoteApiError>>,
    {
        let Response { entries, stats } = block_on(fetch)?;
        let entries = block_on(entries.try_collect())?;
        let stats = block_on(stats)?;
        Ok(Self { entries, stats })
    }
}
