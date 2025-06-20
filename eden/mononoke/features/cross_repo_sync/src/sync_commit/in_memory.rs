/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Context;
use anyhow::Error;
use anyhow::Result;
use anyhow::bail;
use anyhow::format_err;
use changeset_info::ChangesetInfo;
use commit_transformation::CommitRewrittenToEmpty;
use commit_transformation::EmptyCommitFromLargeRepo;
use commit_transformation::RewriteOpts;
use commit_transformation::StripCommitExtras;
use commit_transformation::SubmoduleDeps;
use commit_transformation::SubmoduleExpansionContentIds;
use commit_transformation::git_submodules::InMemoryRepo;
use commit_transformation::git_submodules::SubmoduleExpansionData;
use commit_transformation::rewrite_commit;
use context::CoreContext;
use live_commit_sync_config::LiveCommitSyncConfig;
use metaconfig_types::CommitSyncConfigVersion;
use mononoke_types::BonsaiChangeset;
use mononoke_types::BonsaiChangesetMut;
use mononoke_types::ChangesetId;
use mononoke_types::RepositoryId;
use movers::Movers;
use reporting::CommitSyncContext;
use sorted_vector_map::SortedVectorMap;

use crate::commit_sync_config_utils::get_git_submodule_action_by_version;
use crate::commit_sync_outcome::CommitSyncOutcome;
use crate::commit_syncers_lib::get_movers_by_version;
use crate::commit_syncers_lib::strip_removed_parents;
use crate::commit_syncers_lib::submodule_metadata_file_prefix_and_dangling_pointers;
use crate::commit_syncers_lib::submodule_repos_with_content_ids;
use crate::sync_commit::CommitSyncData;
use crate::sync_config_version_utils::get_mapping_change_version;
use crate::sync_config_version_utils::get_mapping_change_version_from_hg_extra;
use crate::sync_config_version_utils::get_version;
use crate::sync_config_version_utils::get_version_for_merge;
use crate::types::ErrorKind;
use crate::types::Repo;
use crate::types::Source;
use crate::types::Target;

// TODO(T182311609): remove duplication from `CommitSyncOutcome`
#[must_use]
/// Result of running a sync_commit operation but not writing anything to blobstores
/// or database mappings.
pub(crate) enum CommitSyncInMemoryResult {
    NoSyncCandidate {
        source_cs_id: ChangesetId,
        version: CommitSyncConfigVersion,
    },
    Rewritten {
        source_cs_id: ChangesetId,
        rewritten: BonsaiChangesetMut,
        submodule_expansion_content_ids: SubmoduleExpansionContentIds,
        version: CommitSyncConfigVersion,
    },
    WcEquivalence {
        source_cs_id: ChangesetId,
        remapped_id: Option<ChangesetId>,
        version: CommitSyncConfigVersion,
    },
}

impl CommitSyncInMemoryResult {
    /// Write the changes to blobstores and mappings
    pub(crate) async fn write<R: Repo>(
        self,
        ctx: &CoreContext,
        syncer: &CommitSyncData<R>,
    ) -> Result<Option<ChangesetId>, Error> {
        use CommitSyncInMemoryResult::*;
        match self {
            NoSyncCandidate {
                source_cs_id,
                version,
            } => {
                syncer
                    .set_no_sync_candidate(ctx, source_cs_id, version)
                    .await?;
                Ok(None)
            }
            WcEquivalence {
                source_cs_id,
                remapped_id,
                version,
            } => {
                syncer
                    .update_wc_equivalence_with_version(ctx, source_cs_id, remapped_id, version)
                    .await?;
                Ok(None)
            }
            Rewritten {
                source_cs_id,
                rewritten,
                submodule_expansion_content_ids,
                version,
            } => {
                let submodule_content_ids = submodule_repos_with_content_ids(
                    syncer.get_submodule_deps(),
                    submodule_expansion_content_ids,
                )?;

                syncer
                    .upload_rewritten_and_update_mapping(
                        ctx,
                        source_cs_id,
                        rewritten,
                        submodule_content_ids,
                        version,
                    )
                    .await
                    .map(Some)
            }
        }
    }
}

/// Helper struct to do syncing in memory. Doesn't depend on the target repo, except
/// for the repo id.
pub(crate) struct CommitInMemorySyncer<'a, R: Repo> {
    pub ctx: &'a CoreContext,
    pub source_repo: Source<&'a R>,
    pub target_repo_id: Target<RepositoryId>,
    pub submodule_deps: &'a SubmoduleDeps<R>,
    pub live_commit_sync_config: Arc<dyn LiveCommitSyncConfig>,
    pub mapped_parents: &'a HashMap<ChangesetId, CommitSyncOutcome>,
    pub small_to_large: bool,
    /// Read-only version of the large repo, which performs any writes in memory.
    /// This is needed to validate submodule expansion in large repo bonsais.
    pub large_repo: InMemoryRepo,
    pub strip_commit_extras: StripCommitExtras,
    pub should_set_committer_info_to_author_info_if_empty: bool,
    /// Add the name of the commit sync mapping version used to sync the commit
    /// to the rewritten commit's hg extra.
    pub add_mapping_to_hg_extra: bool,
}

// TODO(T182311609): add docs
pub(crate) async fn unsafe_sync_commit_in_memory<'a, R: Repo>(
    ctx: &CoreContext,
    cs: BonsaiChangeset,
    in_memory_syncer: CommitInMemorySyncer<'a, R>,
    commit_sync_context: CommitSyncContext,
    expected_version: Option<CommitSyncConfigVersion>,
) -> Result<CommitSyncInMemoryResult, Error> {
    let maybe_mapping_change_version = get_mapping_change_version(
        &in_memory_syncer
            .source_repo
            .0
            .repo_derived_data()
            .derive::<ChangesetInfo>(in_memory_syncer.ctx, cs.get_changeset_id())
            .await?,
    )?;

    let commit_rewritten_to_empty = in_memory_syncer
        .get_empty_rewritten_commit_action(ctx, &maybe_mapping_change_version, commit_sync_context)
        .await?;

    // We are using the state of pushredirection to determine which repo is "source of truth" for the contents
    // if it's the small repo we can't be rewriting the "mapping change" commits as even if we
    // do they won't be synced back.
    let pushredirection_disabled = !in_memory_syncer
        .live_commit_sync_config
        .push_redirector_enabled_for_public(ctx, in_memory_syncer.target_repo_id.0)
        .await?;

    // During backsyncing we provide an option to skip empty commits but we
    // can only do that when they're not changing the mapping.
    let empty_commit_from_large_repo: EmptyCommitFromLargeRepo = if !in_memory_syncer.small_to_large
        && (maybe_mapping_change_version.is_none() || pushredirection_disabled)
        && justknobs::eval(
            "scm/mononoke:cross_repo_skip_backsyncing_ordinary_empty_commits",
            None,
            Some(in_memory_syncer.source_repo_name().0),
        )
        .unwrap_or(false)
    {
        EmptyCommitFromLargeRepo::Discard
    } else {
        EmptyCommitFromLargeRepo::Keep
    };

    let add_hg_extras = if in_memory_syncer.add_mapping_to_hg_extra {
        SortedVectorMap::from_iter(vec![(
            "sync_mapping_version".to_string(),
            expected_version
                .as_ref()
                .map_or(vec![], |cfg_version| cfg_version.0.clone().into_bytes()),
        )])
    } else {
        Default::default()
    };

    let rewrite_opts = RewriteOpts {
        commit_rewritten_to_empty,
        empty_commit_from_large_repo,
        strip_commit_extras: in_memory_syncer.strip_commit_extras,
        should_set_committer_info_to_author_info_if_empty: in_memory_syncer
            .should_set_committer_info_to_author_info_if_empty,
        add_hg_extras,
    };
    let parent_count = cs.parents().count();
    if parent_count == 0 {
        match expected_version {
            Some(version) => {
                sync_commit_no_parents_in_memory(cs, in_memory_syncer, version, rewrite_opts).await
            }
            None => bail!(
                "no version specified for remapping commit {} with no parents",
                cs.get_changeset_id(),
            ),
        }
    } else if parent_count == 1 {
        sync_commit_single_parent_in_memory(cs, in_memory_syncer, expected_version, rewrite_opts)
            .await
    } else {
        // Syncing merge doesn't take rewrite_opts because merges are always rewritten.
        sync_merge_in_memory(
            ctx,
            cs,
            in_memory_syncer,
            commit_sync_context,
            expected_version,
        )
        .await
    }
}

impl<'a, R: Repo> CommitInMemorySyncer<'a, R> {
    // ------------------------------------------------------------------------
    // Other methods

    /// Determine what should happen to commits that would be empty when synced
    /// to the target repo.
    async fn get_empty_rewritten_commit_action(
        &self,
        ctx: &CoreContext,
        maybe_mapping_change_version: &Option<CommitSyncConfigVersion>,
        commit_sync_context: CommitSyncContext,
    ) -> Result<CommitRewrittenToEmpty> {
        let pushredirection_disabled = !self
            .live_commit_sync_config
            .push_redirector_enabled_for_public(ctx, self.target_repo_id.0)
            .await?;
        // If a commit is changing mapping let's always rewrite it to
        // small repo regardless if outcome is empty. This is to ensure
        // that efter changing mapping there's a commit in small repo
        // with new mapping on top.
        if (maybe_mapping_change_version.is_some() && !pushredirection_disabled)
             ||
             // Initial imports only happen from small to large and might remove
             // file changes to git submodules, which would lead to empty commits.
             // These commits should still be written to the large repo.
             commit_sync_context == CommitSyncContext::ForwardSyncerInitialImport
        {
            Ok(CommitRewrittenToEmpty::Keep)
        } else {
            Ok(CommitRewrittenToEmpty::Discard)
        }
    }

    /// Get `CommitSyncConfigVersion` to use while remapping a
    /// merge commit (`source_cs_id`)
    /// The idea is to derive this version from the `parent_outcomes`
    /// according to the following rules:
    /// - all `NotSyncCandidate` parents are ignored
    /// - all `RewrittenAs` and `EquivalentWorkingCopyAncestor`
    ///   parents have the same (non-None) version associated
    async fn get_movers_to_use_for_merge(
        &self,
        source_cs_id: ChangesetId,
        parent_outcomes: Vec<&CommitSyncOutcome>,
    ) -> Result<(Movers, CommitSyncConfigVersion), Error> {
        let version =
            get_version_for_merge(self.ctx, self.source_repo.0, source_cs_id, parent_outcomes)
                .await?;

        let movers = get_movers_by_version(
            &version,
            Arc::clone(&self.live_commit_sync_config),
            self.source_repo_id(),
            self.target_repo_id,
        )
        .await
        .with_context(|| format!("failed getting a mover of version {}", version))?;
        Ok((movers, version))
    }

    fn source_repo_id(&self) -> Source<RepositoryId> {
        Source(self.source_repo.repo_identity().id())
    }

    fn source_repo_name(&self) -> Source<&str> {
        Source(self.source_repo.repo_identity().name())
    }

    fn small_repo_id(&self) -> RepositoryId {
        if self.small_to_large {
            self.source_repo.0.repo_identity().id()
        } else {
            self.target_repo_id.0
        }
    }
}

// ------------------------------------------------------------------------
// Sync methods

async fn sync_commit_no_parents_in_memory<'a, R: Repo>(
    cs: BonsaiChangeset,
    in_memory_syncer: CommitInMemorySyncer<'a, R>,
    expected_version: CommitSyncConfigVersion,
    rewrite_opts: RewriteOpts,
) -> Result<CommitSyncInMemoryResult, Error> {
    let source_cs_id = cs.get_changeset_id();
    let maybe_version = get_version(
        in_memory_syncer.ctx,
        in_memory_syncer.source_repo.0,
        source_cs_id,
        &[],
    )
    .await?;
    if let Some(version) = maybe_version {
        if version != expected_version {
            return Err(format_err!(
                "computed sync config version {} for {} not the same as expected version {}",
                source_cs_id,
                version,
                expected_version
            ));
        }
    }

    let movers = get_movers_by_version(
        &expected_version,
        Arc::clone(&in_memory_syncer.live_commit_sync_config),
        in_memory_syncer.source_repo_id(),
        in_memory_syncer.target_repo_id,
    )
    .await?;
    let git_submodules_action = get_git_submodule_action_by_version(
        in_memory_syncer.ctx,
        Arc::clone(&in_memory_syncer.live_commit_sync_config),
        &expected_version,
        in_memory_syncer.source_repo_id().0,
        in_memory_syncer.target_repo_id.0,
    )
    .await?;

    let (x_repo_submodule_metadata_file_prefix, dangling_submodule_pointers) =
        submodule_metadata_file_prefix_and_dangling_pointers(
            in_memory_syncer.small_repo_id(),
            &expected_version,
            in_memory_syncer.live_commit_sync_config.clone(),
        )
        .await?;

    let submodule_expansion_data = match in_memory_syncer.submodule_deps {
        SubmoduleDeps::ForSync(deps) => Some(SubmoduleExpansionData {
            submodule_deps: deps,
            x_repo_submodule_metadata_file_prefix: x_repo_submodule_metadata_file_prefix.as_str(),
            small_repo_id: in_memory_syncer.small_repo_id(),
            large_repo: in_memory_syncer.large_repo,
            dangling_submodule_pointers,
        }),
        SubmoduleDeps::NotNeeded | SubmoduleDeps::NotAvailable => None,
    };

    let rewrite_res = rewrite_commit(
        in_memory_syncer.ctx,
        cs.into_mut(),
        &HashMap::new(),
        movers,
        in_memory_syncer.source_repo.0,
        rewrite_opts,
        git_submodules_action,
        submodule_expansion_data,
    )
    .await?;

    match rewrite_res.rewritten {
        Some(rewritten) => Ok(CommitSyncInMemoryResult::Rewritten {
            source_cs_id,
            rewritten,
            submodule_expansion_content_ids: rewrite_res.submodule_expansion_content_ids,
            version: expected_version,
        }),
        None => Ok(CommitSyncInMemoryResult::WcEquivalence {
            source_cs_id,
            remapped_id: None,
            version: expected_version,
        }),
    }
}

async fn sync_commit_single_parent_in_memory<'a, R: Repo>(
    cs: BonsaiChangeset,
    in_memory_syncer: CommitInMemorySyncer<'a, R>,
    expected_version: Option<CommitSyncConfigVersion>,
    rewrite_opts: RewriteOpts,
) -> Result<CommitSyncInMemoryResult, Error> {
    let source_cs_id = cs.get_changeset_id();
    let cs = cs.into_mut();
    let p = cs.parents[0];

    let parent_sync_outcome = in_memory_syncer
        .mapped_parents
        .get(&p)
        .with_context(|| format!("Parent commit {} is not synced yet", p))?
        .clone();

    use CommitSyncOutcome::*;
    match parent_sync_outcome {
        NotSyncCandidate(version) => {
            // If there's not working copy for parent commit then there's no working
            // copy for child either.
            Ok(CommitSyncInMemoryResult::NoSyncCandidate {
                source_cs_id,
                version,
            })
        }
        RewrittenAs(remapped_p, version) | EquivalentWorkingCopyAncestor(remapped_p, version) => {
            let maybe_version = get_version(
                in_memory_syncer.ctx,
                in_memory_syncer.source_repo.0,
                source_cs_id,
                &[version],
            )
            .await?;
            let version = maybe_version
                .ok_or_else(|| format_err!("sync config version not found for {}", source_cs_id))?;

            if let Some(expected_version) = expected_version {
                if expected_version != version {
                    return Err(ErrorKind::UnexpectedVersion {
                        expected_version,
                        actual_version: version,
                        cs_id: source_cs_id,
                    }
                    .into());
                }
            }

            let movers = get_movers_by_version(
                &version,
                Arc::clone(&in_memory_syncer.live_commit_sync_config),
                in_memory_syncer.source_repo_id(),
                in_memory_syncer.target_repo_id,
            )
            .await?;

            let mut remapped_parents = HashMap::new();
            remapped_parents.insert(p, remapped_p);

            let git_submodules_action = get_git_submodule_action_by_version(
                in_memory_syncer.ctx,
                Arc::clone(&in_memory_syncer.live_commit_sync_config),
                &version,
                in_memory_syncer.source_repo_id().0,
                in_memory_syncer.target_repo_id.0,
            )
            .await?;

            let (x_repo_submodule_metadata_file_prefix, dangling_submodule_pointers) =
                submodule_metadata_file_prefix_and_dangling_pointers(
                    in_memory_syncer.small_repo_id(),
                    &version,
                    in_memory_syncer.live_commit_sync_config.clone(),
                )
                .await?;

            let submodule_expansion_data = match in_memory_syncer.submodule_deps {
                SubmoduleDeps::ForSync(deps) => Some(SubmoduleExpansionData {
                    submodule_deps: deps,
                    x_repo_submodule_metadata_file_prefix: x_repo_submodule_metadata_file_prefix
                        .as_str(),
                    small_repo_id: in_memory_syncer.small_repo_id(),
                    large_repo: in_memory_syncer.large_repo,
                    dangling_submodule_pointers,
                }),
                SubmoduleDeps::NotNeeded | SubmoduleDeps::NotAvailable => None,
            };
            let rewrite_res = rewrite_commit(
                in_memory_syncer.ctx,
                cs,
                &remapped_parents,
                movers,
                in_memory_syncer.source_repo.0,
                rewrite_opts,
                git_submodules_action,
                submodule_expansion_data,
            )
            .await?;
            match rewrite_res.rewritten {
                Some(rewritten) => Ok(CommitSyncInMemoryResult::Rewritten {
                    source_cs_id,
                    rewritten,
                    submodule_expansion_content_ids: rewrite_res.submodule_expansion_content_ids,
                    version,
                }),
                None => {
                    // Source commit doesn't rewrite to any target commits.
                    // In that case equivalent working copy is the equivalent working
                    // copy of the parent
                    Ok(CommitSyncInMemoryResult::WcEquivalence {
                        source_cs_id,
                        remapped_id: Some(remapped_p),
                        version,
                    })
                }
            }
        }
    }
}

/// See more details about the algorithm in https://fb.quip.com/s8fYAOxEohtJ
/// A few important notes:
/// 1) Merges are synced only in LARGE -> SMALL direction.
/// 2) If a large repo merge has more than one parent or is introducing any changes on its own
///    in the small repo it will always be synced. If not we sync only if the large repo is
///    source of truth otherwise it would break forward syncer.
async fn sync_merge_in_memory<'a, R: Repo>(
    ctx: &CoreContext,
    cs: BonsaiChangeset,
    in_memory_syncer: CommitInMemorySyncer<'a, R>,
    commit_sync_context: CommitSyncContext,
    expected_version: Option<CommitSyncConfigVersion>,
) -> Result<CommitSyncInMemoryResult, Error> {
    // It's safe to sync merges during initial import because there's no pushrebase going on
    // which allows us to avoid the edge-cases.
    if in_memory_syncer.small_to_large
        && commit_sync_context != CommitSyncContext::ForwardSyncerInitialImport
    {
        bail!("syncing merge commits is supported only in large to small direction");
    }

    let source_cs_id = cs.get_changeset_id();
    let cs = cs.into_mut();

    let sync_outcomes: Vec<_> = cs
        .parents
        .iter()
        .map(|id| {
            anyhow::Ok((
                *id,
                in_memory_syncer
                    .mapped_parents
                    .get(id)
                    .with_context(|| format!("Missing parent {}", id))?
                    .clone(),
            ))
        })
        .collect::<Result<_, Error>>()?;

    // At this point we know that there's at least one parent after big merge. However we still
    // might have a parent that's NotSyncCandidate
    //
    //   B
    //   | \
    //   |  \
    //   R   X  <- new repo was merged, however this repo was not synced at all.
    //   |   |
    //   |   ...
    //   ...
    //   BM  <- Big merge
    //  / \
    //  ...
    //
    // This parents will be completely removed. However when these parents are removed
    // we also need to be careful to strip all copy info

    let mut not_sync_candidate_versions = HashSet::new();

    let new_parents: HashMap<_, _> = sync_outcomes
        .iter()
        .filter_map(|(p, outcome)| {
            use CommitSyncOutcome::*;
            match outcome {
                EquivalentWorkingCopyAncestor(cs_id, _) | RewrittenAs(cs_id, _) => {
                    Some((*p, *cs_id))
                }
                NotSyncCandidate(version) => {
                    not_sync_candidate_versions.insert(version);
                    None
                }
            }
        })
        .collect();

    let cs = strip_removed_parents(cs, new_parents.keys().collect())?;

    if !new_parents.is_empty() {
        // FIXME: Had to turn it to a vector to avoid "One type is more general than the other"
        // errors
        let outcomes = sync_outcomes
            .iter()
            .map(|(_, outcome)| outcome)
            .collect::<Vec<_>>();

        let (movers, version) = in_memory_syncer
            .get_movers_to_use_for_merge(source_cs_id, outcomes)
            .await
            .context("failed getting a mover to use for merge rewriting")?;

        if let Some(expected_version) = expected_version {
            if version != expected_version {
                return Err(ErrorKind::UnexpectedVersion {
                    expected_version,
                    actual_version: version,
                    cs_id: source_cs_id,
                }
                .into());
            }
        }

        let git_submodules_action = get_git_submodule_action_by_version(
            in_memory_syncer.ctx,
            Arc::clone(&in_memory_syncer.live_commit_sync_config),
            &version,
            in_memory_syncer.source_repo_id().0,
            in_memory_syncer.target_repo_id.0,
        )
        .await?;

        let (x_repo_submodule_metadata_file_prefix, dangling_submodule_pointers) =
            submodule_metadata_file_prefix_and_dangling_pointers(
                in_memory_syncer.small_repo_id(),
                &version,
                in_memory_syncer.live_commit_sync_config.clone(),
            )
            .await?;

        let submodule_expansion_data = match in_memory_syncer.submodule_deps {
            SubmoduleDeps::ForSync(deps) => Some(SubmoduleExpansionData {
                submodule_deps: deps,
                x_repo_submodule_metadata_file_prefix: x_repo_submodule_metadata_file_prefix
                    .as_str(),
                small_repo_id: in_memory_syncer.small_repo_id(),
                large_repo: in_memory_syncer.large_repo,
                dangling_submodule_pointers,
            }),
            SubmoduleDeps::NotNeeded | SubmoduleDeps::NotAvailable => None,
        };
        let is_mapping_change = get_mapping_change_version_from_hg_extra(
            cs.hg_extra.iter().map(|(k, v)| (k.as_str(), v.as_slice())),
        )?
        .is_some();
        let is_backsync_when_small_is_source_of_truth = !in_memory_syncer.small_to_large
            && !in_memory_syncer
                .live_commit_sync_config
                .push_redirector_enabled_for_public(ctx, in_memory_syncer.target_repo_id.0)
                .await?;
        let rewrite_res = rewrite_commit(
            in_memory_syncer.ctx,
            cs,
            &new_parents,
            movers,
            in_memory_syncer.source_repo.0,
            Default::default(),
            git_submodules_action,
            submodule_expansion_data,
        )
        .await?;
        match rewrite_res.rewritten {
                Some(rewritten)
                    // We sync the merge commit if-and-only-if:

                    if rewritten.parents.len() > 1 // it rewrites into actual merge commit in target repo OR
                        || !rewritten.file_changes.is_empty() // is bringing changes into target repo by itin_memory_syncer OR
                        || !is_backsync_when_small_is_source_of_truth // the target repo is not source of truth OR
                        || is_mapping_change => // the commit is changing the xrepo mappings
                {
                    Ok(CommitSyncInMemoryResult::Rewritten {
                        source_cs_id,
                        rewritten,
                        submodule_expansion_content_ids: rewrite_res.submodule_expansion_content_ids,
                        version,
                    })
                }
                Some(_) | None => {
                    // We should end up in this branch only if we have a single
                    // parent or the merge or only one merge parent rewrites into target repo - making
                    // it non-merge commit there.
                    let parent_cs_id = new_parents
                        .values()
                        .next()
                        .ok_or_else(|| Error::msg("logic merge: cannot find merge parent"))?;
                    Ok(CommitSyncInMemoryResult::WcEquivalence {
                        source_cs_id,
                        remapped_id: Some(*parent_cs_id),
                        version,
                    })
                }
            }
    } else {
        // All parents of the merge commit are NotSyncCandidate, mark it as NotSyncCandidate
        // as well
        let mut iter = not_sync_candidate_versions.iter();
        let version = match (iter.next(), iter.next()) {
            (Some(_v1), Some(_v2)) => {
                return Err(format_err!(
                    "Too many parent NotSyncCandidate versions: {:?} while syncing {}",
                    not_sync_candidate_versions,
                    source_cs_id
                ));
            }
            (Some(version), None) => version,
            _ => {
                return Err(format_err!(
                    "Can't find parent version for merge commit {}",
                    source_cs_id
                ));
            }
        };

        Ok(CommitSyncInMemoryResult::NoSyncCandidate {
            source_cs_id,
            version: (*version).clone(),
        })
    }
}
