/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;
use std::num::NonZeroU64;
use std::ops::AddAssign;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use anyhow::format_err;
use edenapi::UploadLookupPolicy;
use edenapi::api::SaplingRemoteApi;
use edenapi_types::AnyFileContentId;
use edenapi_types::AnyId;
use edenapi_types::BonsaiChangesetContent;
use edenapi_types::BonsaiFileChange;
use edenapi_types::ContentId;
use edenapi_types::FileType;
use edenapi_types::RepoPathBuf;
use edenapi_types::SnapshotRawData;
use edenapi_types::SnapshotRawFiles;
use edenapi_types::UploadSnapshotResponse;
use futures::TryStreamExt;
use minibytes::Bytes;
use tokio::task;

use crate::snapshot_cache::SharedSnapshotFileCache;
use crate::util::calc_contentid;

/// Statistics for blob downloads tracking different sources using atomic counters
/// This provides thread-safe, lock-free counting for concurrent async operations
///
/// Note: Tracks by blobs (content) rather than file paths, since multiple paths
/// can reference the same blob content. This makes it easier to reason about.
#[derive(Debug, Default)]
pub struct DownloadFileStats {
    /// Number of blobs found on disk with correct content
    blobs_from_disk_state: AtomicUsize,
    /// Number of blobs retrieved from local cache
    blobs_from_local_cache: AtomicUsize,
    /// Number of blobs fetched remotely from server
    blobs_fetched_remotely: AtomicUsize,
}

impl DownloadFileStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Total number of blobs processed
    pub fn total_blobs(&self) -> usize {
        self.blobs_from_disk_state.load(Ordering::Relaxed)
            + self.blobs_from_local_cache.load(Ordering::Relaxed)
            + self.blobs_fetched_remotely.load(Ordering::Relaxed)
    }

    /// Number of blobs found on disk
    pub fn blobs_from_disk_state(&self) -> usize {
        self.blobs_from_disk_state.load(Ordering::Relaxed)
    }

    /// Number of blobs from local cache
    pub fn blobs_from_local_cache(&self) -> usize {
        self.blobs_from_local_cache.load(Ordering::Relaxed)
    }

    /// Number of blobs fetched remotely
    pub fn blobs_fetched_remotely(&self) -> usize {
        self.blobs_fetched_remotely.load(Ordering::Relaxed)
    }

    /// Add stats for a blob found on disk
    pub fn add_disk_blob(&self) {
        self.blobs_from_disk_state.fetch_add(1, Ordering::Relaxed);
    }

    /// Add stats for a blob from cache
    pub fn add_cached_blob(&self) {
        self.blobs_from_local_cache.fetch_add(1, Ordering::Relaxed);
    }

    /// Add stats for a blob fetched remotely
    pub fn add_remote_blob(&self) {
        self.blobs_fetched_remotely.fetch_add(1, Ordering::Relaxed);
    }

    /// Get a snapshot of the current stats as regular usize values
    pub fn snapshot(&self) -> DownloadFileStatsSnapshot {
        DownloadFileStatsSnapshot {
            blobs_from_disk_state: self.blobs_from_disk_state(),
            blobs_from_local_cache: self.blobs_from_local_cache(),
            blobs_fetched_remotely: self.blobs_fetched_remotely(),
        }
    }
}

impl fmt::Display for DownloadFileStats {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Downloaded {} blobs (disk: {}, cache: {}, remote: {})",
            self.total_blobs(),
            self.blobs_from_disk_state(),
            self.blobs_from_local_cache(),
            self.blobs_fetched_remotely()
        )
    }
}

/// A snapshot of download blob stats at a point in time
/// This is useful for returning stats from async functions
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DownloadFileStatsSnapshot {
    /// Number of blobs found on disk with correct content
    pub blobs_from_disk_state: usize,
    /// Number of blobs retrieved from local cache
    pub blobs_from_local_cache: usize,
    /// Number of blobs fetched remotely from server
    pub blobs_fetched_remotely: usize,
}

impl DownloadFileStatsSnapshot {
    /// Total number of blobs processed
    pub fn total_blobs(&self) -> usize {
        self.blobs_from_disk_state + self.blobs_from_local_cache + self.blobs_fetched_remotely
    }
}

impl fmt::Display for DownloadFileStatsSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Downloaded {} blobs (disk: {}, cache: {}, remote: {})",
            self.total_blobs(),
            self.blobs_from_disk_state,
            self.blobs_from_local_cache,
            self.blobs_fetched_remotely
        )
    }
}

impl AddAssign for DownloadFileStatsSnapshot {
    fn add_assign(&mut self, rhs: Self) {
        self.blobs_from_disk_state += rhs.blobs_from_disk_state;
        self.blobs_from_local_cache += rhs.blobs_from_local_cache;
        self.blobs_fetched_remotely += rhs.blobs_fetched_remotely;
    }
}

#[derive(PartialEq, Eq)]
enum TrackedType {
    Tracked,
    Untracked,
}
use TrackedType::*;

struct FileMetadata(RepoPathBuf, FileType, ContentId, TrackedType);
#[derive(Clone)]
struct FileData(ContentId, Bytes);

fn load_files(
    root: &RepoPathBuf,
    rel_path: RepoPathBuf,
    file_type: FileType,
    tracked: TrackedType,
) -> Result<(FileMetadata, FileData)> {
    let mut abs_path = root.clone();
    abs_path.push(&rel_path);
    let abs_path = abs_path.as_repo_path().as_str();
    let content = match file_type {
        FileType::Symlink => {
            let link = std::fs::read_link(abs_path)?;
            let to = link
                .to_str()
                .context("symlink is not valid UTF-8")?
                .as_bytes();
            Bytes::copy_from_slice(to)
        }
        FileType::Regular | FileType::Executable => Bytes::from_owner(std::fs::read(abs_path)?),
    };
    let content_id = calc_contentid(&content);
    Ok((
        FileMetadata(rel_path, file_type, content_id, tracked),
        FileData(content_id, content),
    ))
}

pub async fn upload_snapshot(
    api: &(impl SaplingRemoteApi + ?Sized),
    data: SnapshotRawData,
    custom_duration_secs: Option<u64>,
    copy_from_bubble_id: Option<NonZeroU64>,
    use_bubble: Option<NonZeroU64>,
    labels: Option<Vec<String>>,
) -> Result<UploadSnapshotResponse> {
    upload_snapshot_with_cache(
        api,
        data,
        custom_duration_secs,
        copy_from_bubble_id,
        use_bubble,
        labels,
        None,
    )
    .await
}

/// Upload snapshot with optional local cache support
pub async fn upload_snapshot_with_cache(
    api: &(impl SaplingRemoteApi + ?Sized),
    data: SnapshotRawData,
    custom_duration_secs: Option<u64>,
    copy_from_bubble_id: Option<NonZeroU64>,
    use_bubble: Option<NonZeroU64>,
    labels: Option<Vec<String>>,
    cache: Option<SharedSnapshotFileCache>,
) -> Result<UploadSnapshotResponse> {
    let SnapshotRawData {
        files,
        author,
        hg_parents,
        time,
        tz,
    } = data;
    let SnapshotRawFiles {
        root,
        modified,
        added,
        removed,
        untracked,
        missing,
    } = files;
    let (need_upload, mut upload_data): (Vec<_>, Vec<_>) = modified
        .into_iter()
        .chain(added.into_iter())
        .map(|(p, t)| (p, t, Tracked))
        .chain(
            // TODO(yancouto): Don't upload untracked files if they're too big.
            untracked.into_iter().map(|(p, t)| (p, t, Untracked)),
        )
        // rel_path is relative to the repo root
        .map(|(rel_path, file_type, tracked)| -> anyhow::Result<_> {
            load_files(&root, rel_path.clone(), file_type, tracked)
                .with_context(|| anyhow::anyhow!("Failed to load file {}", rel_path))
        })
        // Let's ignore file not found errors, they might come from transient files that disappeared.
        .filter_map(|res| match res {
            Ok(ok) => Some(Ok(ok)),
            Err(err) => match err.downcast_ref::<std::io::Error>() {
                Some(io_error) if io_error.kind() == std::io::ErrorKind::NotFound => None,
                _ => Some(Err(err)),
            },
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .unzip();

    // Deduplicate upload data
    let mut uniques = BTreeSet::new();
    upload_data.retain(|FileData(content_id, _)| uniques.insert(*content_id));

    // Start caching task concurrently with upload if caching is enabled
    let cache_task = if let Some(cache) = cache.clone() {
        let upload_data_for_cache = upload_data.clone();
        Some(task::spawn(async move {
            for FileData(content_id, data) in upload_data_for_cache {
                if let Err(e) = cache.store_with_content_id(content_id, &data) {
                    tracing::warn!("Failed to cache file content during upload: {}", e);
                }
            }
        }))
    } else {
        None
    };

    let upload_data = upload_data
        .into_iter()
        .map(|FileData(content_id, data)| (AnyFileContentId::ContentId(content_id), data))
        .collect();

    let bubble_id = if let Some(id) = use_bubble {
        // Extend the lifetime of the existing bubble while reusing it
        // Please, see the documentation of ephemeral_extend for more details.
        // If the requested duration is shorter than the existing bubble's lifetime,
        // the lifetime will not be changed.
        // If the requested duration is not provided, the lifetime will be extended to the default lifetime from this moment or
        // remaining lifetime of the existing bubble, whichever is longer.
        // Note: Bubbles with labels remain active even past their expiry time and can be extended successfully.
        // Only bubbles without labels that have expired will cause the request to fail.
        api.ephemeral_extend(id, custom_duration_secs)
            .await
            .context("Failed to extend ephemeral bubble lifetime")?;
        id
    } else {
        api.ephemeral_prepare(custom_duration_secs.map(Duration::from_secs), labels)
            .await
            .context("Failed to create ephemeral bubble")?
            .bubble_id
    };
    let file_content_tokens = {
        let downcast_error = "incorrect upload token, failed to downcast 'token.data.id' to 'AnyId::AnyFileContentId::ContentId' type";
        // upload file contents first, receiving upload tokens
        api.process_files_upload(
            upload_data,
            Some(bubble_id),
            copy_from_bubble_id,
            UploadLookupPolicy::PerformLookup,
        )
        .await?
        .entries
        .try_collect::<Vec<_>>()
        .await?
        .into_iter()
        .map(|token| {
            let content_id = match token.data.id {
                AnyId::AnyFileContentId(AnyFileContentId::ContentId(id)) => id,
                _ => bail!(downcast_error),
            };
            Ok((content_id, token))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?
    };
    let changeset_response = api
        .upload_bonsai_changeset(
            BonsaiChangesetContent {
                hg_parents,
                author,
                time,
                tz,
                extra: vec![],
                file_changes: need_upload
                    .into_iter()
                    .map(|FileMetadata(path, file_type, cid, tracked)| {
                        let upload_token = file_content_tokens
                            .get(&cid)
                            .with_context(|| {
                                format_err!(
                                    "unexpected error: upload token is missing for ContentId({})",
                                    cid
                                )
                            })?
                            .clone();
                        let change = if tracked == Tracked {
                            BonsaiFileChange::Change {
                                file_type,
                                upload_token,
                                copy_info: None, // TODO(yancouto): Add copy info on tracked changes
                            }
                        } else {
                            BonsaiFileChange::UntrackedChange {
                                file_type,
                                upload_token,
                            }
                        };
                        Ok((path, change))
                    })
                    .chain(
                        removed
                            .into_iter()
                            .map(|path| Ok((path, BonsaiFileChange::Deletion))),
                    )
                    .chain(
                        missing
                            .into_iter()
                            .map(|path| Ok((path, BonsaiFileChange::UntrackedDeletion))),
                    )
                    .collect::<anyhow::Result<_>>()?,
                message: "".to_string(),
                is_snapshot: true,
            },
            Some(bubble_id),
        )
        .await
        .context("Failed to create changeset")?;

    // Wait for caching task to complete if it was started
    if let Some(task) = cache_task {
        if let Err(e) = task.await {
            tracing::warn!("Cache task failed: {}", e);
        }
    }

    Ok(UploadSnapshotResponse {
        changeset_token: changeset_response.token,
        bubble_id,
    })
}
