/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

#![allow(dead_code)]
#![allow(clippy::iter_nth_zero, for_loops_over_fallibles)]
#![allow(unexpected_cfgs)]

//! # dag
//!
//! Building blocks for the commit graph used by source control.

mod bsearch;
pub mod config;
pub mod dag;
pub mod default_impl;
mod delegate;
pub mod errors;
mod fmt;
pub mod iddag;
pub mod iddagstore;
pub mod idmap;
mod idset;
mod integrity;
pub(crate) mod lifecycle;
pub mod ops;
pub mod protocol;
#[cfg(any(test, feature = "render"))]
pub mod render;
pub mod segment;
pub mod set;
pub(crate) mod types_ext;
pub mod utils;
mod verlink;
mod vertex_options;

#[cfg(any(test, feature = "indexedlog-backend"))]
pub use dag::Dag;
pub use dag::DagBuilder;
pub use dag_types::CloneData;
pub use dag_types::Group;
pub use dag_types::Id;
pub use dag_types::Location;
pub use dag_types::Vertex;
pub use dag_types::clone;
pub use dag_types::id;
pub use iddag::FirstAncestorConstraint;
pub use iddag::IdDag;
pub use iddag::IdDagAlgorithm;
pub use iddagstore::IdDagStore;
#[cfg(any(test, feature = "indexedlog-backend"))]
pub use idmap::IdMap;
pub use idset::IdList;
pub use idset::IdSet;
pub use idset::OrderedSpan;
pub use ops::DagAlgorithm;
pub use segment::FlatSegment;
pub use segment::IdSegment;
pub use segment::PreparedFlatSegments;
pub use set::Set;
pub use verlink::VerLink;
pub use vertex_options::VertexListWithOptions;
pub use vertex_options::VertexOptions;

pub type Level = u8;
pub type MemIdDag = IdDag<iddagstore::MemStore>;
#[cfg(any(test, feature = "indexedlog-backend"))]
pub type OnDiskIdDag = IdDag<iddagstore::IndexedLogStore>;

// Short aliases for main public types.
pub type IdSetIter<T> = idset::IdSetIter<T>;
pub type IdSpan = idset::Span;
pub use dag::MemDag;
#[cfg(feature = "indexedlog-backend")]
pub use iddagstore::indexedlog_store::describe_indexedlog_entry;
pub use set::NameIter as SetIter;

#[cfg(any(test, feature = "indexedlog-backend"))]
pub mod tests;

pub use errors::DagError as Error;
pub type Result<T> = std::result::Result<T, Error>;

// Re-export
#[cfg(feature = "indexedlog-backend")]
pub use indexedlog::Repair;
pub use nonblocking;

#[macro_export]
macro_rules! failpoint {
    ($name:literal) => {
        ::fail::fail_point!($name, |_| {
            let msg = format!("failpoint injected by FAILPOINTS: {}", $name);
            Err($crate::errors::DagError::from(
                $crate::errors::BackendError::Generic(msg),
            ))
        })
    };
}

/// Whether running inside a test.
pub(crate) fn is_testing() -> bool {
    std::env::var("TESTTMP").is_ok()
}

#[cfg(test)]
dev_logger::init!();
