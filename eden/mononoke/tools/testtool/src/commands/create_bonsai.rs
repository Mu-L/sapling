/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::Context;
use anyhow::Error;
use anyhow::Result;
use anyhow::anyhow;
use bytes::Bytes;
use changesets_creation::save_changesets;
use clap::Parser;
use mercurial_derivation::MappedHgChangesetId;
use mononoke_app::MononokeApp;
use mononoke_app::args::RepoArgs;
use mononoke_types::BonsaiChangeset;
use mononoke_types::BonsaiChangesetMut;
use mononoke_types::ChangesetId;
use mononoke_types::DateTime;
use mononoke_types::FileChange;
use mononoke_types::NonRootMPath;
use mononoke_types::hash::GitSha1;
use repo_blobstore::RepoBlobstoreRef;
use repo_derived_data::RepoDerivedDataRef;
use serde_derive::Deserialize;
use smallvec::SmallVec;
use sorted_vector_map::SortedVectorMap;

use crate::repo::Repo;

/// Create commits from a JSON-encoded bonsai changeset
///
/// The bonsai changeset is intentionally not checked for correctness, as this
/// may be used in tests to test handling of malformed bonsai changesets.
#[derive(Parser)]
pub struct CommandArgs {
    #[clap(flatten)]
    repo_args: RepoArgs,

    /// Path to a file containing a JSON-encoded bonsai changeset
    bonsai_file: PathBuf,
}

pub async fn run(app: MononokeApp, args: CommandArgs) -> Result<()> {
    let ctx = app.new_basic_context();

    let mut content = String::new();
    File::open(&args.bonsai_file)
        .with_context(|| {
            format!(
                "Failed to open bonsai changeset file '{}'",
                args.bonsai_file.to_string_lossy()
            )
        })?
        .read_to_string(&mut content)
        .context("Failed to read bonsai changeset file")?;

    let bcs: BonsaiChangeset = serde_json::from_str::<DeserializableBonsaiChangeset>(&content)
        .context("Failed to parse bonsai changeset data")?
        .into_bonsai()?
        .freeze()?;

    let repo: Repo = app
        .open_repo(&args.repo_args)
        .await
        .context("Failed to open repo")?;

    for (_, change) in bcs.simplified_file_changes() {
        match change {
            Some(tc) => {
                if filestore::get_metadata(repo.repo_blobstore(), &ctx, &tc.content_id().into())
                    .await?
                    .is_none()
                {
                    return Err(anyhow!(
                        "file content {} is not found in the filestore",
                        &tc.content_id()
                    ));
                }
            }
            None => {}
        }
    }
    let bcs_id = bcs.get_changeset_id();
    save_changesets(&ctx, &repo, vec![bcs])
        .await
        .context("Failed to save changeset")?;
    let hg_cs = repo
        .repo_derived_data()
        .derive::<MappedHgChangesetId>(&ctx, bcs_id)
        .await
        .context("Failed to derive Mercurial changeset")?
        .hg_changeset_id();
    println!(
        "Created bonsai changeset {} for Hg changeset {}",
        bcs_id, hg_cs
    );
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct DeserializableBonsaiChangeset {
    pub parents: Vec<ChangesetId>,
    pub author: String,
    pub author_date: DateTime,
    pub committer: Option<String>,
    pub committer_date: Option<DateTime>,
    pub message: String,
    pub hg_extra: BTreeMap<String, Vec<u8>>,
    pub git_extra_headers: Option<BTreeMap<Vec<u8>, Vec<u8>>>,
    pub git_tree_hash: Option<String>, // hex-encoded
    pub file_changes: BTreeMap<String, FileChange>,
}

impl DeserializableBonsaiChangeset {
    pub fn into_bonsai(self) -> Result<BonsaiChangesetMut, Error> {
        let file_changes = self
            .file_changes
            .into_iter()
            .map::<Result<_, Error>, _>(|(path, changes)| {
                Ok((NonRootMPath::new(path.as_bytes())?, changes))
            })
            .collect::<Result<SortedVectorMap<_, _>, _>>()?;
        let git_extra_headers = self.git_extra_headers.map(|extra| {
            extra
                .into_iter()
                .map(|(k, v)| (SmallVec::from(k), Bytes::from(v)))
                .collect()
        });
        let git_tree_hash = self
            .git_tree_hash
            .as_deref()
            .map(GitSha1::from_str)
            .transpose()?;
        Ok(BonsaiChangesetMut {
            parents: self.parents,
            author: self.author,
            author_date: self.author_date,
            committer: self.committer,
            committer_date: self.committer_date,
            message: self.message,
            hg_extra: self.hg_extra.into(),
            git_extra_headers,
            file_changes,
            is_snapshot: false,
            git_annotated_tag: None,
            git_tree_hash,
            subtree_changes: Default::default(),
        })
    }
}
