/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

use anyhow::Result;
use cloned::cloned;
use futures_util::FutureExt;
use futures_util::future;
use maplit::btreeset;
use metaconfig_types::CommitIdentityScheme;
use mononoke_api::ChangesetContext;
use mononoke_api::ChangesetId;
use mononoke_api::MononokeError;
use mononoke_api::Repo;
use mononoke_api::RepoContext;
use source_control as thrift;

/// Generate a mapping for a commit's identity into the requested identity
/// schemes.
pub(crate) async fn map_commit_identity(
    changeset_ctx: &ChangesetContext<Repo>,
    schemes: &BTreeSet<thrift::CommitIdentityScheme>,
) -> Result<BTreeMap<thrift::CommitIdentityScheme, thrift::CommitId>, MononokeError> {
    let mut ids = BTreeMap::new();
    ids.insert(
        thrift::CommitIdentityScheme::BONSAI,
        thrift::CommitId::bonsai(changeset_ctx.id().as_ref().into()),
    );

    let schemes = fall_back_to_default_identity_scheme(changeset_ctx.repo_ctx(), schemes);

    let mut scheme_identities = vec![];
    if schemes.contains(&thrift::CommitIdentityScheme::HG) {
        let identity = async {
            if let Some(hg_id) = changeset_ctx.hg_id().await? {
                let result: Result<Option<_>, MononokeError> = Ok(Some((
                    thrift::CommitIdentityScheme::HG,
                    thrift::CommitId::hg(hg_id.as_ref().into()),
                )));
                result
            } else {
                Ok(None)
            }
        };
        scheme_identities.push(identity.boxed());
    }
    if schemes.contains(&thrift::CommitIdentityScheme::GLOBALREV) {
        let identity = async {
            if let Some(globalrev) = changeset_ctx.globalrev().await? {
                let result: Result<Option<_>, MononokeError> = Ok(Some((
                    thrift::CommitIdentityScheme::GLOBALREV,
                    thrift::CommitId::globalrev(globalrev.id() as i64),
                )));
                result
            } else {
                Ok(None)
            }
        };
        scheme_identities.push(identity.boxed());
    }
    if schemes.contains(&thrift::CommitIdentityScheme::SVNREV) {
        let identity = async {
            if let Some(svnrev) = changeset_ctx.svnrev().await? {
                let result: Result<Option<_>, MononokeError> = Ok(Some((
                    thrift::CommitIdentityScheme::SVNREV,
                    thrift::CommitId::svnrev(svnrev.id() as i64),
                )));
                result
            } else {
                Ok(None)
            }
        };
        scheme_identities.push(identity.boxed());
    }
    if schemes.contains(&thrift::CommitIdentityScheme::GIT) {
        let identity = async {
            if let Some(git_sha1) = changeset_ctx.git_sha1().await? {
                let result: Result<Option<_>, MononokeError> = Ok(Some((
                    thrift::CommitIdentityScheme::GIT,
                    thrift::CommitId::git(git_sha1.as_ref().into()),
                )));
                result
            } else {
                Ok(None)
            }
        };
        scheme_identities.push(identity.boxed());
    }
    let scheme_identities = future::try_join_all(scheme_identities).await?;
    for (scheme, id) in scheme_identities.into_iter().flatten() {
        ids.insert(scheme, id);
    }
    Ok(ids)
}

/// Generate mappings for multiple commits' identities into the requested
/// identity schemes.
pub(crate) async fn map_commit_identities(
    repo_ctx: &RepoContext<Repo>,
    ids: Vec<ChangesetId>,
    schemes: &BTreeSet<thrift::CommitIdentityScheme>,
) -> Result<
    BTreeMap<ChangesetId, BTreeMap<thrift::CommitIdentityScheme, thrift::CommitId>>,
    MononokeError,
> {
    let mut result = BTreeMap::new();
    for id in ids.iter() {
        let mut idmap = BTreeMap::new();
        idmap.insert(
            thrift::CommitIdentityScheme::BONSAI,
            thrift::CommitId::bonsai(id.as_ref().into()),
        );
        result.insert(*id, idmap);
    }

    let schemes = fall_back_to_default_identity_scheme(repo_ctx, schemes);

    let mut scheme_identities = vec![];
    if schemes.contains(&thrift::CommitIdentityScheme::HG) {
        let ids = ids.clone();
        let identities = async {
            let bonsai_hg_ids = repo_ctx
                .many_changeset_hg_ids(ids)
                .await?
                .into_iter()
                .map(|(cs_id, hg_cs_id)| {
                    (
                        cs_id,
                        thrift::CommitIdentityScheme::HG,
                        thrift::CommitId::hg(hg_cs_id.as_ref().into()),
                    )
                })
                .collect::<Vec<_>>();
            let result: Result<_, MononokeError> = Ok(bonsai_hg_ids);
            result
        };
        scheme_identities.push(identities.boxed());
    }
    if schemes.contains(&thrift::CommitIdentityScheme::GIT) {
        cloned!(ids);
        let identities = async {
            let bonsai_git_shas = repo_ctx
                .many_changeset_git_sha1s(ids)
                .await?
                .into_iter()
                .map(|(cs_id, git_sha1)| {
                    (
                        cs_id,
                        thrift::CommitIdentityScheme::GIT,
                        thrift::CommitId::git(git_sha1.as_ref().into()),
                    )
                })
                .collect::<Vec<_>>();
            let result: Result<_, MononokeError> = Ok(bonsai_git_shas);
            result
        };
        scheme_identities.push(identities.boxed());
    }
    if schemes.contains(&thrift::CommitIdentityScheme::GLOBALREV) {
        cloned!(ids);
        let identities = async {
            let bonsai_globalrev_ids = repo_ctx
                .many_changeset_globalrev_ids(ids)
                .await?
                .into_iter()
                .map(|(cs_id, globalrev)| {
                    (
                        cs_id,
                        thrift::CommitIdentityScheme::GLOBALREV,
                        thrift::CommitId::globalrev(globalrev.id() as i64),
                    )
                })
                .collect::<Vec<_>>();
            let result: Result<_, MononokeError> = Ok(bonsai_globalrev_ids);
            result
        };
        scheme_identities.push(identities.boxed());
    }
    if schemes.contains(&thrift::CommitIdentityScheme::SVNREV) {
        let identities = async {
            let bonsai_svnrev_ids = repo_ctx
                .many_changeset_svnrev_ids(ids)
                .await?
                .into_iter()
                .map(|(cs_id, svnrev)| {
                    (
                        cs_id,
                        thrift::CommitIdentityScheme::SVNREV,
                        thrift::CommitId::svnrev(svnrev.id() as i64),
                    )
                })
                .collect::<Vec<_>>();
            let result: Result<_, MononokeError> = Ok(bonsai_svnrev_ids);
            result
        };
        scheme_identities.push(identities.boxed());
    }
    let scheme_identities = future::try_join_all(scheme_identities).await?;
    for ids in scheme_identities {
        for (cs_id, commit_identity_scheme, commit_id) in ids {
            result
                .entry(cs_id)
                .or_insert_with(BTreeMap::new)
                .insert(commit_identity_scheme, commit_id);
        }
    }
    Ok(result)
}

/// If identity schemes were not provided, get the repo's default identity scheme
/// and use it.
fn fall_back_to_default_identity_scheme<'a>(
    repo_ctx: &RepoContext<Repo>,
    schemes: &'a BTreeSet<thrift::CommitIdentityScheme>,
) -> Cow<'a, BTreeSet<thrift::CommitIdentityScheme>> {
    if !schemes.is_empty() {
        // If identity schemes were specified by the user, return them
        return Cow::Borrowed(schemes);
    };

    let use_default_id_scheme = justknobs::eval(
        "scm/mononoke:use_repo_default_id_scheme_in_scs",
        None,
        Some(repo_ctx.name()),
    )
    .unwrap_or(false);

    if !use_default_id_scheme {
        // If feature is disabled or identity schemes were specified by the
        // user, return the provided schemes.
        return Cow::Borrowed(schemes);
    };

    // Otherwise, get the repo's default identity scheme and use it.
    let default_scheme = repo_ctx.config().default_commit_identity_scheme.clone();

    let maybe_translated_scheme = match default_scheme {
        CommitIdentityScheme::HG => Some(thrift::CommitIdentityScheme::HG),
        CommitIdentityScheme::GIT => Some(thrift::CommitIdentityScheme::GIT),
        CommitIdentityScheme::BONSAI => Some(thrift::CommitIdentityScheme::BONSAI),
        _ => None,
    };
    match maybe_translated_scheme {
        Some(translated_scheme) => Cow::Owned(btreeset! {translated_scheme}),
        None => Cow::Borrowed(schemes),
    }
}

/// Trait to extend CommitId with useful functions.
pub(crate) trait CommitIdExt {
    fn scheme(&self) -> thrift::CommitIdentityScheme;
}

impl CommitIdExt for thrift::CommitId {
    /// Returns the commit identity scheme of a commit ID.
    fn scheme(&self) -> thrift::CommitIdentityScheme {
        match self {
            thrift::CommitId::bonsai(_) => thrift::CommitIdentityScheme::BONSAI,
            thrift::CommitId::ephemeral_bonsai(_) => thrift::CommitIdentityScheme::EPHEMERAL_BONSAI,
            thrift::CommitId::hg(_) => thrift::CommitIdentityScheme::HG,
            thrift::CommitId::git(_) => thrift::CommitIdentityScheme::GIT,
            thrift::CommitId::globalrev(_) => thrift::CommitIdentityScheme::GLOBALREV,
            thrift::CommitId::svnrev(_) => thrift::CommitIdentityScheme::SVNREV,
            thrift::CommitId::UnknownField(t) => (*t).into(),
        }
    }
}
