/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use bookmarks::BookmarkKey;
use bookmarks::BookmarksRef;
use cas_client::build_mononoke_cas_client;
use changesets_uploader::CasChangesetsUploader;
use changesets_uploader::PriorLookupPolicy;
use changesets_uploader::UploadPolicy;
use clap::Args;
use context::CoreContext;
use mercurial_derivation::RootHgAugmentedManifestId;
use mercurial_types::HgChangesetId;
use metaconfig_types::RepoConfigRef;
use mononoke_types::ChangesetId;
use mononoke_types::MPath;
use repo_derived_data::RepoDerivedDataRef;
use repo_identity::RepoIdentityRef;
use scuba_ext::MononokeScubaSampleBuilder;
use slog::info;

const SCUBA_TABLE: &str = "mononoke_cas_ttl_walker";

use super::Repo;

#[derive(Args)]
/// Subcommand to upload (augmented) tree and blob data into the cas store.
/// This command can also upload the entire changeset.
pub struct CasStoreUploadArgs {
    /// Bonsai changeset id that needs to be uploaded into the cas store.
    #[clap(long, short = 'i')]
    changeset_id: Option<ChangesetId>,
    /// Hg changeset id that needs to be uploaded into the cas store.
    #[clap(long)]
    hg_id: Option<HgChangesetId>,
    /// Bookmark pointing to the changeset that needs to be uploaded into the cas store.
    #[clap(long, short = 'B')]
    bookmark: Option<BookmarkKey>,
    /// Upload the entire changeset's working copy data recursively.
    #[clap(long)]
    full: bool,
    /// Upload only the specified path (allowed for full uploads only)
    #[clap(long, short, requires = "full")]
    path: Option<String>,
    /// Verbose logging of the upload process (CAS) vs quiet output by default.
    #[clap(long)]
    verbose: bool,
    /// Upload only the blobs of a changeset.
    #[clap(long)]
    blobs_only: bool,
    /// Upload only the trees of a changeset.
    #[clap(long, conflicts_with = "blobs_only")]
    trees_only: bool,
}

pub async fn cas_store_upload(
    ctx: &CoreContext,
    repo: &Repo,
    args: CasStoreUploadArgs,
) -> Result<()> {
    let use_case = repo
        .repo_config()
        .mononoke_cas_sync_config
        .as_ref()
        .ok_or_else(|| {
            anyhow!(
                "Missing mononoke_cas_sync_config config for repo: {}",
                repo.repo_identity().name()
            )
        })?
        .use_case_public
        .as_ref();

    let cas_changesets_uploader = CasChangesetsUploader::new(build_mononoke_cas_client(
        ctx.fb,
        ctx.clone(),
        repo.repo_identity().name(),
        args.verbose,
        use_case,
    )?);

    // Resolve the changeset id
    let changeset_id = match args.changeset_id {
        Some(changeset_id) => Ok(changeset_id),
        None => match args.hg_id {
            Some(hg_id) => repo
                .bonsai_hg_mapping
                .get_bonsai_from_hg(ctx, hg_id)
                .await?
                .ok_or(anyhow!("No bonsai changeset found for hg id {}", hg_id)),
            None => {
                if let Some(bookmark) = args.bookmark {
                    repo.bookmarks()
                        .get(ctx.clone(), &bookmark)
                        .await?
                        .ok_or(anyhow!(
                            "No changeset found for bookmark {}",
                            bookmark.name()
                        ))
                } else {
                    Err(anyhow!(
                        "No changeset id provided. Either hg or bonsai changeset id must be provided or a bookmark name to point to the changeset."
                    ))
                }
            }
        },
    }?;

    let upload_policy = if args.trees_only {
        UploadPolicy::TreesOnly
    } else if args.blobs_only {
        UploadPolicy::BlobsOnly
    } else {
        UploadPolicy::All
    };

    let mut path = None;
    if let Some(ref spath) = args.path {
        path = Some(MPath::new(spath).with_context(|| anyhow!("Invalid path: {}", spath))?);
    }

    // Derive augmented manifest for this changeset if not yet derived.
    repo.repo_derived_data()
        .derive::<RootHgAugmentedManifestId>(ctx, changeset_id)
        .await?;

    let stats = if args.full {
        let mut scuba_sample = MononokeScubaSampleBuilder::new(ctx.fb, SCUBA_TABLE)?;
        scuba_sample.add_common_server_data();

        let time = std::time::Instant::now();
        let stats = cas_changesets_uploader
            .upload_single_changeset_recursively(
                ctx,
                repo,
                &changeset_id,
                path,
                upload_policy,
                PriorLookupPolicy::All,
            )
            .await?;

        // The number of digests that were not present in the CAS store.
        scuba_sample.add("uploaded_digests", stats.uploaded_digests());
        // The number of files we uploaded to the CAS store during this walk.
        scuba_sample.add("uploaded_files", stats.uploaded_files());
        // The number of trees we uploaded to the CAS store during this walk.
        scuba_sample.add("uploaded_trees", stats.uploaded_trees());
        // The number of bytes we uploaded to the CAS store during this walk.
        scuba_sample.add("uploaded_bytes", stats.uploaded_bytes());
        // The number of digests that were already present in the CAS store.
        scuba_sample.add("present_digests", stats.already_present_digests());
        // The number of files that were already present in the CAS store.
        scuba_sample.add("present_files", stats.already_present_files());
        // The number of trees that were already present in the CAS store.
        scuba_sample.add("present_trees", stats.already_present_trees());
        // The repo name.
        scuba_sample.add("repo_name", repo.repo_identity.name());
        // The duration of the walk.
        scuba_sample.add("walk_duration_sec", time.elapsed().as_secs());
        scuba_sample.log();
        stats
    } else {
        cas_changesets_uploader
            .upload_single_changeset(
                ctx,
                repo,
                &changeset_id,
                upload_policy,
                PriorLookupPolicy::All,
            )
            .await?
    };

    info!(ctx.logger(), "Upload completed. Upload stats: {}", stats);

    Ok(())
}
