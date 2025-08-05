/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::collections::HashMap;
use std::collections::HashSet;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use async_trait::async_trait;
use clientinfo::ClientEntryPoint;
use clientinfo::ClientInfo;
use context::CoreContext;
use edenapi::Client;
use edenapi::HttpClientBuilder;
use edenapi::HttpClientConfig;
use edenapi::SaplingRemoteApi;
use edenapi::api::UploadLookupPolicy;
use edenapi::paths;
use edenapi_types::AnyFileContentId;
use edenapi_types::AnyId;
use edenapi_types::LookupResponse;
use edenapi_types::LookupResult;
use edenapi_types::UploadToken;
use edenapi_types::UploadTokenData;
use edenapi_types::bookmark::Freshness;
use futures::StreamExt;
use futures::TryStreamExt;
use futures::stream;
use http_client::HttpVersion;
use mercurial_types::HgChangesetId;
use mercurial_types::HgFileNodeId;
use mercurial_types::HgManifestId;
use mercurial_types::blobs::HgBlobChangeset;
use minibytes::Bytes;
use mononoke_types::BonsaiChangeset;
use mononoke_types::ChangesetId;
use repo_blobstore::RepoBlobstore;

use crate::sender::edenapi::EdenapiConfig;
use crate::sender::edenapi::EdenapiSender;
use crate::sender::edenapi::util;
use crate::stat;

pub struct DefaultEdenapiSenderBuilder {
    ctx: CoreContext,
    config: EdenapiConfig,
    reponame: String,
    repo_blobstore: RepoBlobstore,
}

impl DefaultEdenapiSenderBuilder {
    pub fn new(
        ctx: CoreContext,
        config: EdenapiConfig,
        reponame: String,
        repo_blobstore: RepoBlobstore,
    ) -> Self {
        Self {
            ctx,
            config,
            reponame,
            repo_blobstore,
        }
    }

    pub async fn build(self) -> Result<DefaultEdenapiSender> {
        let tls_args = self.config.tls_args.clone();
        let ci = ClientInfo::new_with_entry_point(ClientEntryPoint::ModernSync)
            .with_context(|| "building client info")?
            .to_json()
            .with_context(|| "converting to json")?;
        let http_config = HttpClientConfig {
            cert_path: Some(tls_args.tls_certificate.into()),
            key_path: Some(tls_args.tls_private_key.into()),
            ca_path: Some(tls_args.tls_ca.into()),
            client_info: Some(ci),
            ..Default::default()
        };

        let timeout = justknobs::get_as::<u64>("scm/mononoke:modern_sync_edenapi_timeout", None)
            .unwrap_or(300);

        tracing::info!(
            "Connecting to {}, timeout {}s",
            self.config.url.to_string(),
            timeout
        );

        let client = HttpClientBuilder::new()
            .repo_name(&self.reponame)
            .server_url(self.config.url.clone())
            .http_config(http_config.clone())
            .http_version(HttpVersion::V11)
            .timeout(Duration::from_secs(timeout))
            .build()
            .with_context(|| "building http client")?;

        client
            .health()
            .await
            .with_context(|| "checking client health")?;

        Ok(DefaultEdenapiSender {
            ctx: self.ctx.clone(),
            repo_blobstore: self.repo_blobstore,
            client,
        })
    }
}

pub struct DefaultEdenapiSender {
    ctx: CoreContext,
    client: Client,
    repo_blobstore: RepoBlobstore,
}

#[async_trait]
impl EdenapiSender for DefaultEdenapiSender {
    async fn upload_contents(&self, contents: Vec<(AnyFileContentId, Bytes)>) -> Result<()> {
        let ctx = self.ctx.clone();

        let expected_responses = contents.len();
        let response = self
            .client
            .process_files_upload(contents, None, None, UploadLookupPolicy::SkipLookup)
            .await
            .with_context(|| "uploading contents")?;

        let ids = response
            .entries
            .try_collect::<Vec<_>>()
            .await
            .with_context(|| "collecting contents entries")?
            .iter()
            .map(|e| e.data.id)
            .collect::<Vec<_>>();

        if let Ok(stats) = response.stats.await {
            stat::log_edenapi_stats(ctx.scuba().clone(), &stats, paths::UPLOAD_FILE, ids.clone());
        }

        ensure!(
            expected_responses == ids.len(),
            "Content upload: Expected {} responses, got {}",
            expected_responses,
            ids.len()
        );

        Ok(())
    }

    async fn upload_trees(&self, trees: Vec<HgManifestId>) -> Result<()> {
        let batch_len = trees.len();
        let entries = stream::iter(trees.clone())
            .map(|mf_id| {
                let ctx = self.ctx.clone();
                let repo_blobstore = self.repo_blobstore.clone();
                async move { util::from_tree_to_entry(mf_id, &ctx, &repo_blobstore).await }
            })
            // Modern sync controls the size of the dequeue batch, so we
            // can read all the tree blobs concurrently.
            .buffer_unordered(batch_len)
            .try_collect::<Vec<_>>()
            .await
            .with_context(|| "uploading trees")?;

        let expected_responses = entries.len();
        let res = self.client.upload_trees_batch(entries).await?;
        let ids = res
            .entries
            .try_collect::<Vec<_>>()
            .await
            .with_context(|| "collecting trees entries")?
            .iter()
            .map(|e| e.token.data.id)
            .collect::<Vec<_>>();

        if let Ok(stats) = res.stats.await {
            stat::log_edenapi_stats(
                self.ctx.scuba().clone(),
                &stats,
                paths::UPLOAD_TREES,
                ids.clone(),
            );
        }

        ensure!(
            expected_responses == ids.len(),
            "Trees upload: Expected {} responses, got {}",
            expected_responses,
            ids.len(),
        );
        Ok(())
    }

    async fn upload_filenodes(&self, fn_ids: Vec<HgFileNodeId>) -> Result<()> {
        let batch_len = fn_ids.len();
        let filenodes = stream::iter(fn_ids)
            .map(|file_id| {
                let ctx = self.ctx.clone();
                let repo_blobstore = self.repo_blobstore.clone();
                async move { util::from_id_to_filenode(file_id, &ctx, &repo_blobstore).await }
            })
            // Modern sync controls the size of the dequeue batch,
            // so we can read all the filenode blobs concurrently.
            .buffer_unordered(batch_len)
            .try_collect::<Vec<_>>()
            .await
            .with_context(|| "uploading filenodes")?;

        let expected_responses = filenodes.len();
        let res = self.client.upload_filenodes_batch(filenodes).await?;
        let ids = res
            .entries
            .try_collect::<Vec<_>>()
            .await
            .with_context(|| "collecting filenodes entries")?
            .iter()
            .map(|e| e.token.data.id)
            .collect::<Vec<_>>();

        if let Ok(stats) = res.stats.await {
            stat::log_edenapi_stats(
                self.ctx.scuba().clone(),
                &stats,
                paths::UPLOAD_FILENODES,
                ids.clone(),
            );
        }

        ensure!(
            expected_responses == ids.len(),
            "Filenodes upload: Expected {} responses, got {}",
            expected_responses,
            ids.len()
        );
        Ok(())
    }

    async fn set_bookmark(
        &self,
        bookmark: String,
        from: Option<HgChangesetId>,
        to: Option<HgChangesetId>,
    ) -> Result<()> {
        let res = self
            .client
            .set_bookmark(
                bookmark,
                to.map(|cs| cs.into()),
                from.map(|cs| cs.into()),
                HashMap::from([
                    ("BYPASS_READONLY".to_owned(), "true".to_owned()),
                    ("MIRROR_UPLOAD".to_owned(), "true".to_owned()),
                ]),
            )
            .await
            .with_context(|| "setting bookmark")?;
        tracing::info!("Moved bookmark with result {:?}", res);
        Ok(())
    }

    async fn upload_identical_changeset(
        &self,
        css: Vec<(HgBlobChangeset, BonsaiChangeset)>,
    ) -> Result<()> {
        let entries = stream::iter(css)
            .map(util::to_identical_changeset)
            .try_collect::<Vec<_>>()
            .await
            .with_context(|| "collecting changeset entries")?;

        let expected_responses = entries.len();
        let res = self
            .client
            .upload_identical_changesets(entries)
            .await
            .with_context(|| "uploading changesets")?;

        let responses = res
            .entries
            .try_collect::<Vec<_>>()
            .await
            .with_context(|| "collecting changesets responses")?;
        ensure!(
            expected_responses == responses.len(),
            "Not all changesets were uploaded"
        );
        let ids = responses
            .iter()
            .map(|r| r.token.data.id)
            .collect::<Vec<_>>();

        if let Ok(stats) = res.stats.await {
            stat::log_edenapi_stats(
                self.ctx.scuba().clone(),
                &stats,
                paths::UPLOAD_IDENTICAL_CHANGESET,
                ids.clone(),
            );
        }

        tracing::info!("Uploaded changesets: {:?}", ids);

        Ok(())
    }

    async fn filter_existing_commits(
        &self,
        ids: Vec<(HgChangesetId, ChangesetId)>,
    ) -> Result<Vec<ChangesetId>> {
        let hgids = ids
            .clone()
            .iter()
            .map(|(hgid, _)| AnyId::HgChangesetId(hgid.clone().into()))
            .collect::<Vec<_>>();
        let res = self
            .client
            .lookup_batch(hgids, None, None)
            .await
            .with_context(|| "filtering existing commits")?;
        let missing = get_missing_in_order(res, ids);
        Ok(missing)
    }

    async fn read_bookmark(&self, bookmark: String) -> Result<Option<HgChangesetId>> {
        let res = self
            .client
            .bookmarks2(vec![bookmark], Some(Freshness::MostRecent))
            .await
            .with_context(|| "reading bookmark")?;

        Ok(res
            .into_iter()
            .next()
            .map(|entry| {
                anyhow::Ok(
                    entry
                        .data
                        .with_context(|| "extracting bookmark response")?
                        .hgid,
                )
            })
            .transpose()
            .with_context(|| "processing bookmark response")?
            .flatten()
            .map(|id| id.into()))
    }
}

fn get_missing_in_order(
    lookup_res: Vec<LookupResponse>,
    ids: Vec<(HgChangesetId, ChangesetId)>,
) -> Vec<ChangesetId> {
    let present_ids: HashSet<_> = lookup_res
        .into_iter()
        .filter_map(|r| match r.result {
            LookupResult::Present(UploadToken {
                data:
                    UploadTokenData {
                        id,
                        bubble_id: _,
                        metadata: _,
                    },
                signature: _,
            }) => Some(id),
            _ => None,
        })
        .collect();

    let missing: Vec<_> = ids
        .into_iter()
        .filter(|(hgid, _)| !present_ids.contains(&AnyId::HgChangesetId((*hgid).into())))
        .map(|(_, csid)| csid)
        .collect();
    missing
}

#[cfg(test)]
mod test {

    use edenapi_types::IndexableId;
    use edenapi_types::LookupResponse;
    use mononoke_macros::mononoke;

    use super::*;

    #[mononoke::test]
    fn test_mpath_element_size() {
        let cs_id1 = ChangesetId::from_bytes([0; 32]).unwrap();
        let hg_id1 = HgChangesetId::from_bytes(&[1; 20]).unwrap();

        let cs_id2 = ChangesetId::from_bytes([1; 32]).unwrap();
        let hg_id2 = HgChangesetId::from_bytes(&[2; 20]).unwrap();

        let response1 = LookupResponse {
            result: LookupResult::NotPresent(IndexableId {
                id: AnyId::BonsaiChangesetId(cs_id1.into()),
                bubble_id: None,
            }),
        };

        let response2 = LookupResponse {
            result: LookupResult::NotPresent(IndexableId {
                id: AnyId::BonsaiChangesetId(cs_id2.into()),
                bubble_id: None,
            }),
        };

        // Simulate responses in inverted order
        let responses = vec![response2, response1];

        // This should preserve the ids order
        let missing = get_missing_in_order(responses, vec![(hg_id1, cs_id1), (hg_id2, cs_id2)]);
        assert_eq!(missing, vec![cs_id1, cs_id2]);
    }
}
