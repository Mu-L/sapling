/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Error;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use blobstore::Loadable;
use bonsai_git_mapping::BonsaiGitMapping;
use bonsai_hg_mapping::BonsaiHgMapping;
use bookmarks::BookmarkUpdateLog;
use bookmarks::Bookmarks;
use changesets_creation::save_changesets;
use commit_graph::CommitGraph;
use commit_graph::CommitGraphWriter;
use context::CoreContext;
use fbinit::FacebookInit;
use filestore::FilestoreConfig;
use maplit::btreemap;
use maplit::hashmap;
use maplit::hashset;
use megarepo_configs::SourceMappingRules;
use mononoke_macros::mononoke;
use mononoke_types::ChangesetId;
use mononoke_types::ContentId;
use mononoke_types::FileChange;
use mononoke_types::FileType;
use mononoke_types::GitLfs;
use mononoke_types::NonRootMPath;
use mononoke_types::path::MPath;
use repo_blobstore::RepoBlobstore;
use repo_blobstore::RepoBlobstoreRef;
use repo_cross_repo::RepoCrossRepo;
use repo_derived_data::RepoDerivedData;
use repo_identity::RepoIdentity;
use sorted_vector_map::SortedVectorMap;
use test_repo_factory::TestRepoFactory;
use tests_utils::CreateCommitContext;
use tests_utils::list_working_copy_utf8;

use super::*;
use crate::commit_rewriting::create_source_to_target_multi_mover;
use crate::implicit_deletes::minimize_file_change_set;
use crate::types::FileChangeFilter;
use crate::types::FileChangeFilterApplication;
use crate::types::FileChangeFilterFunc;

#[facet::container]
#[derive(Clone)]
pub struct Repo(
    RepoIdentity,
    RepoBlobstore,
    dyn Bookmarks,
    dyn BonsaiHgMapping,
    RepoDerivedData,
    CommitGraph,
    dyn CommitGraphWriter,
    FilestoreConfig,
    dyn BookmarkUpdateLog,
    dyn BonsaiGitMapping,
    RepoCrossRepo,
);

#[mononoke::test]
fn test_multi_mover_simple() -> Result<(), Error> {
    let mapping_rules = SourceMappingRules {
        default_prefix: "".to_string(),
        ..Default::default()
    };
    let multi_mover = create_source_to_target_multi_mover(mapping_rules)?;
    assert_eq!(
        multi_mover.multi_move_path(&NonRootMPath::new("path")?)?,
        vec![NonRootMPath::new("path")?]
    );
    Ok(())
}

#[mononoke::test]
fn test_multi_mover_prefixed() -> Result<(), Error> {
    let mapping_rules = SourceMappingRules {
        default_prefix: "prefix".to_string(),
        ..Default::default()
    };
    let multi_mover = create_source_to_target_multi_mover(mapping_rules)?;
    assert_eq!(
        multi_mover.multi_move_path(&NonRootMPath::new("path")?)?,
        vec![NonRootMPath::new("prefix/path")?]
    );
    Ok(())
}

#[mononoke::test]
fn test_multi_mover_prefixed_with_exceptions() -> Result<(), Error> {
    let mapping_rules = SourceMappingRules {
        default_prefix: "prefix".to_string(),
        overrides: btreemap! {
            "override".to_string() => vec![
                "overriden_1".to_string(),
                "overriden_2".to_string(),
            ]
        },
        ..Default::default()
    };
    let multi_mover = create_source_to_target_multi_mover(mapping_rules)?;
    assert_eq!(
        multi_mover.multi_move_path(&NonRootMPath::new("path")?)?,
        vec![NonRootMPath::new("prefix/path")?]
    );

    assert_eq!(
        multi_mover.multi_move_path(&NonRootMPath::new("override/path")?)?,
        vec![
            NonRootMPath::new("overriden_1/path")?,
            NonRootMPath::new("overriden_2/path")?,
        ]
    );
    Ok(())
}

#[mononoke::test]
fn test_multi_mover_longest_prefix_first() -> Result<(), Error> {
    let mapping_rules = SourceMappingRules {
        default_prefix: "prefix".to_string(),
        overrides: btreemap! {
            "prefix".to_string() => vec![
                "prefix_1".to_string(),
            ],
            "prefix/sub".to_string() => vec![
                "prefix/sub_1".to_string(),
            ]
        },
        ..Default::default()
    };
    let multi_mover = create_source_to_target_multi_mover(mapping_rules)?;
    assert_eq!(
        multi_mover.multi_move_path(&NonRootMPath::new("prefix/path")?)?,
        vec![NonRootMPath::new("prefix_1/path")?]
    );

    assert_eq!(
        multi_mover.multi_move_path(&NonRootMPath::new("prefix/sub/path")?)?,
        vec![NonRootMPath::new("prefix/sub_1/path")?]
    );

    Ok(())
}

fn path(p: &str) -> NonRootMPath {
    NonRootMPath::new(p).unwrap()
}

fn verify_minimized(changes: Vec<(&str, Option<()>)>, expected: BTreeMap<&str, Option<()>>) {
    fn to_file_change(o: Option<()>) -> FileChange {
        match o {
            Some(_) => FileChange::tracked(
                ContentId::from_bytes([1; 32]).unwrap(),
                FileType::Regular,
                0,
                None,
                GitLfs::FullContent,
            ),
            None => FileChange::Deletion,
        }
    }
    let changes: Vec<_> = changes
        .into_iter()
        .map(|(p, c)| (path(p), to_file_change(c)))
        .collect();
    let minimized = minimize_file_change_set(changes);
    let expected: SortedVectorMap<NonRootMPath, FileChange> = expected
        .into_iter()
        .map(|(p, c)| (path(p), to_file_change(c)))
        .collect();
    assert_eq!(expected, minimized);
}

#[mononoke::fbinit_test]
fn test_minimize_file_change_set(_fb: FacebookInit) {
    verify_minimized(
        vec![("a", Some(())), ("a", None)],
        btreemap! { "a" => Some(())},
    );
    verify_minimized(vec![("a", Some(()))], btreemap! { "a" => Some(())});
    verify_minimized(vec![("a", None)], btreemap! { "a" => None});
    // directories are deleted implicitly, so explicit deletes are
    // minimized away
    verify_minimized(
        vec![("a/b", None), ("a/c", None), ("a", Some(()))],
        btreemap! { "a" => Some(()) },
    );
    // files, replaced with a directly at a longer path are not
    // deleted implicitly, so they aren't minimized away
    verify_minimized(
        vec![("a", None), ("a/b", Some(()))],
        btreemap! { "a" => None, "a/b" => Some(()) },
    );
}

#[mononoke::fbinit_test]
async fn test_rewrite_commit_marks_lossy_conversions(fb: FacebookInit) -> Result<(), Error> {
    let repo: Repo = TestRepoFactory::new(fb)?.build().await?;
    let ctx = CoreContext::test_mock(fb);
    let mapping_rules = SourceMappingRules {
        default_prefix: "".to_string(), // Rewrite to root
        overrides: btreemap! {
            "www".to_string() => vec!["".to_string()], // map changes to www to root
            "xplat".to_string() => vec![], // swallow changes outside of www
        },
        ..Default::default()
    };
    let multi_mover = create_source_to_target_multi_mover(mapping_rules)?;
    // Add files to www and xplat (lossy)
    let first = CreateCommitContext::new_root(&ctx, &repo)
        .add_file("www/foo.php", "foo content")
        .add_file("www/bar/baz.php", "baz content")
        .add_file("www/bar/crux.php", "crux content")
        .add_file("xplat/a/a.js", "a content")
        .add_file("xplat/a/b.js", "b content")
        .add_file("xplat/b/c.js", "c content")
        .commit()
        .await?;
    // Only add one file in xplat (No changeset will be generated)
    let second = CreateCommitContext::new(&ctx, &repo, vec![first])
        .add_file("xplat/c/d.js", "d content")
        .commit()
        .await?;
    // Only add one file in www (non-lossy)
    let third = CreateCommitContext::new(&ctx, &repo, vec![second])
        .add_file("www/baz/foobar.php", "foobar content")
        .commit()
        .await?;
    // Only change files in www (non-lossy)
    let fourth = CreateCommitContext::new(&ctx, &repo, vec![third])
        .add_file("www/baz/foobar.php", "more foobar content")
        .add_file("www/foo.php", "more foo content")
        .commit()
        .await?;
    // Only delete files in www (non-lossy)
    let fifth = CreateCommitContext::new(&ctx, &repo, vec![fourth])
        .delete_file("www/baz/crux.php")
        .commit()
        .await?;
    // Delete files in www and xplat (lossy)
    let sixth = CreateCommitContext::new(&ctx, &repo, vec![fifth])
        .delete_file("xplat/a/a.js")
        .delete_file("www/bar/baz.php")
        .commit()
        .await?;

    let first_rewritten_bcs_id = test_rewrite_commit_cs_id(
        &ctx,
        &repo,
        first,
        HashMap::new(),
        multi_mover.clone(),
        None,
    )
    .await?;
    assert_changeset_is_marked_lossy(&ctx, &repo, first_rewritten_bcs_id).await?;

    assert!(
        test_rewrite_commit_cs_id(
            &ctx,
            &repo,
            second,
            hashmap! {
                first => first_rewritten_bcs_id,
            },
            multi_mover.clone(),
            None,
        )
        .await
        .is_err()
    );

    let third_rewritten_bcs_id = test_rewrite_commit_cs_id(
        &ctx,
        &repo,
        third,
        hashmap! {
            second => first_rewritten_bcs_id, // there is no second equivalent
        },
        multi_mover.clone(),
        None,
    )
    .await?;
    assert_changeset_is_not_marked_lossy(&ctx, &repo, third_rewritten_bcs_id).await?;

    let fourth_rewritten_bcs_id = test_rewrite_commit_cs_id(
        &ctx,
        &repo,
        fourth,
        hashmap! {
            third => third_rewritten_bcs_id,
        },
        multi_mover.clone(),
        None,
    )
    .await?;
    assert_changeset_is_not_marked_lossy(&ctx, &repo, fourth_rewritten_bcs_id).await?;

    let fifth_rewritten_bcs_id = test_rewrite_commit_cs_id(
        &ctx,
        &repo,
        fifth,
        hashmap! {
            fourth => fourth_rewritten_bcs_id,
        },
        multi_mover.clone(),
        None,
    )
    .await?;
    assert_changeset_is_not_marked_lossy(&ctx, &repo, fifth_rewritten_bcs_id).await?;

    let sixth_rewritten_bcs_id = test_rewrite_commit_cs_id(
        &ctx,
        &repo,
        sixth,
        hashmap! {
            fifth => fifth_rewritten_bcs_id,
        },
        multi_mover.clone(),
        None,
    )
    .await?;
    assert_changeset_is_marked_lossy(&ctx, &repo, sixth_rewritten_bcs_id).await?;

    Ok(())
}

#[mononoke::fbinit_test]
async fn test_rewrite_commit_marks_lossy_conversions_with_implicit_deletes(
    fb: FacebookInit,
) -> Result<(), Error> {
    let repo: Repo = TestRepoFactory::new(fb)?.build().await?;
    let ctx = CoreContext::test_mock(fb);
    // This commit is not lossy because all paths will be mapped somewhere.
    let first = CreateCommitContext::new_root(&ctx, &repo)
        .add_file("a/b/c/d", "d")
        .add_file("a/b/c/e", "e")
        .add_file("a/b/c/f/g", "g")
        .add_file("a/b/c/f/h", "h")
        .add_file("a/b/c/i", "i")
        .commit()
        .await?;
    let second = CreateCommitContext::new(&ctx, &repo, vec![first])
        .add_file("a/b/c", "new c") // This creates a file at `a/b/c`, implicitly deleting the directory
        // at `a/b/c` and all the files it contains (`d`, `e`, `f/g` and `f/h`)
        .add_file("a/b/i", "new i")
        .commit()
        .await?;

    // With the full mapping rules, all directories from the repo have a mapping
    let full_mapping_rules = SourceMappingRules {
        default_prefix: "".to_string(),
        overrides: btreemap! {
            "a/b".to_string() => vec!["ab".to_string()],
            "a/b/c".to_string() => vec!["abc".to_string()],
            "a/b/c/f".to_string() => vec!["abcf".to_string()],
        },
        ..Default::default()
    };
    let full_multi_mover = create_source_to_target_multi_mover(full_mapping_rules)?;
    // With the partial mapping rules, files under `a/b/c/f` don't have a mapping
    let partial_mapping_rules = SourceMappingRules {
        default_prefix: "".to_string(),
        overrides: btreemap! {
            "a/b".to_string() => vec!["ab".to_string()],
            "a/b/c".to_string() => vec!["abc".to_string()],
            "a/b/c/f".to_string() => vec![],
        },
        ..Default::default()
    };
    let partial_multi_mover = create_source_to_target_multi_mover(partial_mapping_rules)?;

    // We rewrite the first commit with the full_multi_mover.
    // This is not lossy.
    let first_rewritten_bcs_id = test_rewrite_commit_cs_id(
        &ctx,
        &repo,
        first,
        HashMap::new(),
        full_multi_mover.clone(),
        None,
    )
    .await?;
    assert_changeset_is_not_marked_lossy(&ctx, &repo, first_rewritten_bcs_id).await?;
    // When we rewrite the second commit with the full_multi_mover.
    // This is not lossy:
    // All file changes have a mapping and all implicitly deleted files have a mapping.
    let full_second_rewritten_bcs_id = test_rewrite_commit_cs_id(
        &ctx,
        &repo,
        second,
        hashmap! {
            first => first_rewritten_bcs_id
        },
        full_multi_mover.clone(),
        None,
    )
    .await?;
    assert_changeset_is_not_marked_lossy(&ctx, &repo, full_second_rewritten_bcs_id).await?;
    // When we rewrite the second commit with the partial_multi_mover.
    // This **is** lossy:
    // All file changes have a mapping but some implicitly deleted files don't have a mapping
    // (namely, `a/b/c/f/g` and `a/b/c/f/h`).
    let partial_second_rewritten_bcs_id = test_rewrite_commit_cs_id(
        &ctx,
        &repo,
        second,
        hashmap! {
            first => first_rewritten_bcs_id
        },
        partial_multi_mover.clone(),
        None,
    )
    .await?;
    assert_changeset_is_marked_lossy(&ctx, &repo, partial_second_rewritten_bcs_id).await?;
    Ok(())
}

#[mononoke::fbinit_test]
async fn test_rewrite_commit(fb: FacebookInit) -> Result<(), Error> {
    let repo: Repo = TestRepoFactory::new(fb)?.build().await?;
    let ctx = CoreContext::test_mock(fb);
    let first = CreateCommitContext::new_root(&ctx, &repo)
        .add_file("path", "path")
        .commit()
        .await?;
    let second = CreateCommitContext::new(&ctx, &repo, vec![first])
        .add_file_with_copy_info("pathsecondcommit", "pathsecondcommit", (first, "path"))
        .commit()
        .await?;
    let third = CreateCommitContext::new(&ctx, &repo, vec![first, second])
        .add_file("path", "pathmodified")
        .commit()
        .await?;

    let mapping_rules = SourceMappingRules {
        default_prefix: "prefix".to_string(),
        overrides: btreemap! {
            "path".to_string() => vec![
                "path_1".to_string(),
                "path_2".to_string(),
            ]
        },
        ..Default::default()
    };
    let multi_mover = create_source_to_target_multi_mover(mapping_rules)?;

    let first_rewritten_bcs_id = test_rewrite_commit_cs_id(
        &ctx,
        &repo,
        first,
        HashMap::new(),
        multi_mover.clone(),
        None,
    )
    .await?;

    let first_rewritten_wc = list_working_copy_utf8(&ctx, &repo, first_rewritten_bcs_id).await?;
    assert_eq!(
        first_rewritten_wc,
        hashmap! {
            NonRootMPath::new("path_1")? => "path".to_string(),
            NonRootMPath::new("path_2")? => "path".to_string(),
        }
    );

    let second_rewritten_bcs_id = test_rewrite_commit_cs_id(
        &ctx,
        &repo,
        second,
        hashmap! {
            first => first_rewritten_bcs_id
        },
        multi_mover.clone(),
        None,
    )
    .await?;

    let second_bcs = second_rewritten_bcs_id
        .load(&ctx, &repo.repo_blobstore())
        .await?;
    let maybe_copy_from = match second_bcs
        .file_changes_map()
        .get(&NonRootMPath::new("prefix/pathsecondcommit")?)
        .ok_or_else(|| anyhow!("path not found"))?
    {
        FileChange::Change(tc) => tc.copy_from().cloned(),
        _ => bail!("path_is_deleted"),
    };

    assert_eq!(
        maybe_copy_from,
        Some((NonRootMPath::new("path_1")?, first_rewritten_bcs_id))
    );

    let second_rewritten_wc = list_working_copy_utf8(&ctx, &repo, second_rewritten_bcs_id).await?;
    assert_eq!(
        second_rewritten_wc,
        hashmap! {
            NonRootMPath::new("path_1")? => "path".to_string(),
            NonRootMPath::new("path_2")? => "path".to_string(),
            NonRootMPath::new("prefix/pathsecondcommit")? => "pathsecondcommit".to_string(),
        }
    );

    // Diamond merge test with error during parent reordering
    assert!(
        test_rewrite_commit_cs_id(
            &ctx,
            &repo,
            third,
            hashmap! {
                first => first_rewritten_bcs_id,
                second => second_rewritten_bcs_id
            },
            multi_mover.clone(),
            Some(second), // wrong, should be after-rewrite id
        )
        .await
        .is_err()
    );

    // Diamond merge test with success
    let third_rewritten_bcs_id = test_rewrite_commit_cs_id(
        &ctx,
        &repo,
        third,
        hashmap! {
            first => first_rewritten_bcs_id,
            second => second_rewritten_bcs_id
        },
        multi_mover,
        Some(second_rewritten_bcs_id),
    )
    .await?;

    let third_bcs = third_rewritten_bcs_id
        .load(&ctx, &repo.repo_blobstore().clone())
        .await?;

    assert_eq!(
        third_bcs.parents().collect::<Vec<_>>(),
        vec![second_rewritten_bcs_id, first_rewritten_bcs_id],
    );

    Ok(())
}

/**
 * Set up a small repo to test multiple scenarios with file change filters.
 *
 * The first commit sets the following structure:
 * foo
 *  └── bar
 *      ├── a
 *      ├── b
 *      │   ├── d
 *      │   └── e
 *      └── c
 *          ├── f
 *          └── g
 *
 * The second commit adds two files `foo/bar/b` (executable) and `foo/bar/c`
 * which implicitly deletes some files under `foo/bar`.
 */
async fn test_rewrite_commit_with_file_changes_filter(
    fb: FacebookInit,
    file_change_filters: Vec<FileChangeFilter<'_>>,
    mut expected_affected_paths: HashMap<&str, HashSet<NonRootMPath>>,
) -> Result<(), Error> {
    let repo: Repo = TestRepoFactory::new(fb)?.build().await?;

    let ctx = CoreContext::test_mock(fb);
    // This commit is not lossy because all paths will be mapped somewhere.
    let first = CreateCommitContext::new_root(&ctx, &repo)
        .add_file("foo/bar/a", "a")
        .add_file("foo/bar/b/d", "d")
        .add_file("foo/bar/b/e", "e")
        .add_file("foo/bar/c/f", "f")
        .add_file("foo/bar/c/g", "g")
        .commit()
        .await?;

    // Create files at `foo/bar/b` and `foo/bar/c`, implicitly deleting all
    // files under those directories.
    let second = CreateCommitContext::new(&ctx, &repo, vec![first])
        // Implicitly deletes `foo/bar/b/d` and `foo/bar/b/e`.
        // Adding it as an executable so we can test filters that apply on
        // conditions other than paths.
        .add_file_with_type("foo/bar/b", "new b", FileType::Executable)
        // Implicitly deletes `foo/bar/c/f` and `foo/bar/c/g`.
        .add_file("foo/bar/c", "new c")
        .commit()
        .await?;

    struct IdentityMultiMover;

    impl MultiMover for IdentityMultiMover {
        fn multi_move_path(&self, path: &NonRootMPath) -> Result<Vec<NonRootMPath>, Error> {
            Ok(vec![path.clone()])
        }

        fn conflicts_with(&self, _path: &NonRootMPath) -> Result<bool> {
            Ok(true)
        }
    }

    let identity_multi_mover = Arc::new(IdentityMultiMover);

    async fn verify_affected_paths(
        ctx: &CoreContext,
        repo: &Repo,
        rewritten_bcs_id: &ChangesetId,
        expected_affected_paths: HashSet<NonRootMPath>,
    ) -> Result<()> {
        let bcs = rewritten_bcs_id.load(ctx, repo.repo_blobstore()).await?;

        let affected_paths = bcs
            .file_changes()
            .map(|(p, _fc)| p.clone())
            .collect::<HashSet<_>>();

        assert_eq!(expected_affected_paths, affected_paths);
        Ok(())
    }

    let first_rewritten_bcs_id = test_rewrite_commit_cs_id_with_file_change_filters(
        &ctx,
        &repo,
        first,
        HashMap::new(),
        identity_multi_mover.clone(),
        None,
        file_change_filters.clone(),
    )
    .await?;

    verify_affected_paths(
        &ctx,
        &repo,
        &first_rewritten_bcs_id,
        expected_affected_paths.remove("first").unwrap(),
    )
    .await?;

    let second_rewritten_bcs_id = test_rewrite_commit_cs_id_with_file_change_filters(
        &ctx,
        &repo,
        second,
        hashmap! {
            first => first_rewritten_bcs_id
        },
        identity_multi_mover.clone(),
        None,
        file_change_filters,
    )
    .await?;

    verify_affected_paths(
        &ctx,
        &repo,
        &second_rewritten_bcs_id,
        expected_affected_paths.remove("second").unwrap(),
    )
    .await?;

    Ok(())
}

/// Tests applying a file change filter before getting the implicit deletes
/// and calling the multi mover.
#[mononoke::fbinit_test]
async fn test_rewrite_commit_with_file_changes_filter_on_both_based_on_path(
    fb: FacebookInit,
) -> Result<(), Error> {
    let file_change_filter_func: FileChangeFilterFunc<'_> =
        Arc::new(|(source_path, _): (&NonRootMPath, &FileChange)| -> bool {
            let ignored_path_prefix: NonRootMPath = NonRootMPath::new("foo/bar/b").unwrap();
            !ignored_path_prefix.is_prefix_of(source_path)
        });

    let file_change_filters: Vec<FileChangeFilter<'_>> = vec![FileChangeFilter {
        func: file_change_filter_func,
        application: FileChangeFilterApplication::Both,
    }];

    let expected_affected_paths: HashMap<&str, HashSet<NonRootMPath>> = hashmap! {
        // Changes to `foo/bar/b/d` and `foo/bar/b/e` are removed in the
        // final bonsai because the filter ran before the multi-mover.
        "first" => hashset! {
            NonRootMPath::new("foo/bar/a").unwrap(),
            NonRootMPath::new("foo/bar/c/f").unwrap(),
            NonRootMPath::new("foo/bar/c/g").unwrap()
        },
        // We expect only the added file to be affected. The delete of
        // `foo/bar/c/g` and `foo/bar/c/f` will remain implicit because
        // the change to `foo/bar/c` is present in the bonsai.
        "second" => hashset! {
            NonRootMPath::new("foo/bar/c").unwrap()
        },
    };

    test_rewrite_commit_with_file_changes_filter(fb, file_change_filters, expected_affected_paths)
        .await?;

    Ok(())
}

/// Tests applying a file change filter before getting the implicit deletes
/// and calling the multi mover.
#[mononoke::fbinit_test]
async fn test_rewrite_commit_with_file_changes_filter_on_both_based_on_file_type(
    fb: FacebookInit,
) -> Result<(), Error> {
    let file_change_filter_func: FileChangeFilterFunc<'_> =
        Arc::new(|(_, fc): (&NonRootMPath, &FileChange)| -> bool {
            match fc {
                FileChange::Change(tfc) => tfc.file_type() != FileType::Executable,
                _ => true,
            }
        });

    let file_change_filters: Vec<FileChangeFilter<'_>> = vec![FileChangeFilter {
        func: file_change_filter_func,
        application: FileChangeFilterApplication::Both,
    }];

    let expected_affected_paths: HashMap<&str, HashSet<NonRootMPath>> = hashmap! {
         // All changes are synced because there are no executable files.
         "first" => hashset! {
            NonRootMPath::new("foo/bar/a").unwrap(),
            NonRootMPath::new("foo/bar/c/f").unwrap(),
            NonRootMPath::new("foo/bar/c/g").unwrap(),
            NonRootMPath::new("foo/bar/b/e").unwrap(),
            NonRootMPath::new("foo/bar/b/d").unwrap(),
        },
        // We expect only the added file to be affected. The delete of
        // `foo/bar/c/g` and `foo/bar/c/f` will remain implicit because
        // the change to `foo/bar/c` is present in the bonsai.
        // The files under `foo/bar/b` will not be implicitly or explicitly
        // deleted because the addition of the executable file was ignored
        // when getting the implicit deletes and rewriting the changes.
        "second" => hashset! {
            NonRootMPath::new("foo/bar/c").unwrap()
        },
    };

    test_rewrite_commit_with_file_changes_filter(fb, file_change_filters, expected_affected_paths)
        .await?;

    Ok(())
}

/// Tests applying a file change filter only before getting the
/// implicit deletes.
#[mononoke::fbinit_test]
async fn test_rewrite_commit_with_file_changes_filter_implicit_deletes_only(
    fb: FacebookInit,
) -> Result<(), Error> {
    let file_change_filter_func: FileChangeFilterFunc<'_> =
        Arc::new(|(source_path, _): (&NonRootMPath, &FileChange)| -> bool {
            let ignored_path_prefix: NonRootMPath = NonRootMPath::new("foo/bar/b").unwrap();
            !ignored_path_prefix.is_prefix_of(source_path)
        });

    let file_change_filters: Vec<FileChangeFilter<'_>> = vec![FileChangeFilter {
        func: file_change_filter_func,
        application: FileChangeFilterApplication::ImplicitDeletes,
    }];
    // Applying the filter only before the implicit deletes should increase
    // performance because it won't do unnecessary work, but it should NOT
    // affect which file changes are synced.
    // That's because even if implicit deletes are found, because no filter
    // is applied before the multi-mover, they will still be expressed
    // implicitly in the final bonsai.
    let expected_affected_paths: HashMap<&str, HashSet<NonRootMPath>> = hashmap! {
        // Since the filter for `foo/bar/b` is applied only before getting
        // the implicit deletes, all changes will be synced.
        "first" => hashset! {
            NonRootMPath::new("foo/bar/a").unwrap(),
            NonRootMPath::new("foo/bar/b/d").unwrap(),
            NonRootMPath::new("foo/bar/b/e").unwrap(),
            NonRootMPath::new("foo/bar/c/f").unwrap(),
            NonRootMPath::new("foo/bar/c/g").unwrap()
        },
        // The same applies to the second commit. The same paths are synced.
        "second" => hashset! {
            NonRootMPath::new("foo/bar/c").unwrap(),
            // The path file added that implicitly deletes the two above
            NonRootMPath::new("foo/bar/b").unwrap(),
            // `foo/bar/b/d` and `foo/bar/b/e` will not be present in the
            // bonsai, because they're being deleted implicitly.
            //
            // WHY: the filter is applied only when getting the implicit deletes.
            // So `foo/bar/b` is synced via the multi mover, which means that
            // the delete is already expressed implicitly, so `minimize_file_change_set`
            // will remove the unnecessary explicit deletes.
        },
    };

    test_rewrite_commit_with_file_changes_filter(fb, file_change_filters, expected_affected_paths)
        .await?;

    Ok(())
}

/// Tests applying a file change filter only before calling the
/// multi mover.
/// This test uses the file type as the filter condition, to showcase
/// a more realistic scenario where we only want to apply the filter to
/// the multi mover.
#[mononoke::fbinit_test]
async fn test_rewrite_commit_with_file_changes_filter_multi_mover_only(
    fb: FacebookInit,
) -> Result<(), Error> {
    let file_change_filter_func: FileChangeFilterFunc<'_> =
        Arc::new(|(_, fc): (&NonRootMPath, &FileChange)| -> bool {
            match fc {
                FileChange::Change(tfc) => tfc.file_type() != FileType::Executable,
                _ => true,
            }
        });
    let file_change_filters: Vec<FileChangeFilter<'_>> = vec![FileChangeFilter {
        func: file_change_filter_func,
        application: FileChangeFilterApplication::MultiMover,
    }];

    let expected_affected_paths: HashMap<&str, HashSet<NonRootMPath>> = hashmap! {
        // All changes are synced because there are no executable files.
        "first" => hashset! {
            NonRootMPath::new("foo/bar/a").unwrap(),
            NonRootMPath::new("foo/bar/c/f").unwrap(),
            NonRootMPath::new("foo/bar/c/g").unwrap(),
            NonRootMPath::new("foo/bar/b/e").unwrap(),
            NonRootMPath::new("foo/bar/b/d").unwrap(),
        },
        "second" => hashset! {
            NonRootMPath::new("foo/bar/c").unwrap(),
            // `foo/bar/b` implicitly deletes these two files below in the
            // source bonsai. However, because the change to `foo/bar/b`
            // will not be synced (is't an executable file), these implicit
            // deletes will be added explicitly to the rewritten bonsai.
            NonRootMPath::new("foo/bar/b/e").unwrap(),
            NonRootMPath::new("foo/bar/b/d").unwrap(),
        },
    };

    test_rewrite_commit_with_file_changes_filter(fb, file_change_filters, expected_affected_paths)
        .await?;

    Ok(())
}

async fn test_rewrite_commit_cs_id<'a>(
    ctx: &'a CoreContext,
    repo: &'a Repo,
    bcs_id: ChangesetId,
    parents: HashMap<ChangesetId, ChangesetId>,
    multi_mover: Arc<dyn MultiMover + 'a>,
    force_first_parent: Option<ChangesetId>,
) -> Result<ChangesetId, Error> {
    test_rewrite_commit_cs_id_with_file_change_filters(
        ctx,
        repo,
        bcs_id,
        parents,
        multi_mover,
        force_first_parent,
        vec![],
    )
    .await
}

async fn test_rewrite_commit_cs_id_with_file_change_filters<'a>(
    ctx: &'a CoreContext,
    repo: &'a Repo,
    bcs_id: ChangesetId,
    parents: HashMap<ChangesetId, ChangesetId>,
    multi_mover: Arc<dyn MultiMover + 'a>,
    force_first_parent: Option<ChangesetId>,
    file_change_filters: Vec<FileChangeFilter<'a>>,
) -> Result<ChangesetId, Error> {
    let bcs = bcs_id.load(ctx, &repo.repo_blobstore()).await?;
    let bcs = bcs.into_mut();

    let maybe_rewritten = rewrite_commit_with_file_changes_filter(
        ctx,
        bcs,
        &parents,
        multi_mover,
        repo,
        force_first_parent,
        Default::default(),
        file_change_filters,
    )
    .await?;
    let rewritten = maybe_rewritten.ok_or_else(|| anyhow!("can't rewrite commit {}", bcs_id))?;
    let rewritten = rewritten.freeze()?;

    save_changesets(ctx, repo, vec![rewritten.clone()]).await?;

    Ok(rewritten.get_changeset_id())
}

async fn assert_changeset_is_marked_lossy<'a>(
    ctx: &'a CoreContext,
    repo: &'a Repo,
    bcs_id: ChangesetId,
) -> Result<(), Error> {
    let bcs = bcs_id.load(ctx, &repo.repo_blobstore()).await?;
    assert!(
        bcs.hg_extra()
            .any(|(key, _)| key == "created_by_lossy_conversion"),
        "Failed with {:?}",
        bcs
    );
    Ok(())
}

async fn assert_changeset_is_not_marked_lossy<'a>(
    ctx: &'a CoreContext,
    repo: &'a Repo,
    bcs_id: ChangesetId,
) -> Result<(), Error> {
    let bcs = bcs_id.load(ctx, &repo.repo_blobstore()).await?;
    assert!(
        !bcs.hg_extra()
            .any(|(key, _)| key == "created_by_lossy_conversion"),
        "Failed with {:?}",
        bcs
    );
    Ok(())
}

#[mononoke::test]
fn test_directory_multi_mover() -> Result<(), Error> {
    let mapping_rules = SourceMappingRules {
        default_prefix: "prefix".to_string(),
        ..Default::default()
    };
    let multi_mover = create_directory_source_to_target_multi_mover(mapping_rules)?;
    assert_eq!(
        multi_mover(&MPath::new("path")?)?,
        vec![MPath::new("prefix/path")?]
    );

    assert_eq!(multi_mover(&MPath::ROOT)?, vec![MPath::new("prefix")?]);
    Ok(())
}

#[mononoke::fbinit_test]
async fn test_rewrite_lfs_file(fb: FacebookInit) -> Result<(), Error> {
    let repo: Repo = TestRepoFactory::new(fb)?.build().await?;
    let ctx = CoreContext::test_mock(fb);
    let mapping_rules = SourceMappingRules {
        default_prefix: "small".to_string(),
        ..Default::default()
    };
    let multi_mover = create_source_to_target_multi_mover(mapping_rules)?;
    // Add an LFS file to the repo
    let first = CreateCommitContext::new_root(&ctx, &repo)
        .add_file("foo.php", "foo content")
        .add_file_with_type_and_lfs(
            "large.avi",
            "large file content",
            FileType::Regular,
            GitLfs::canonical_pointer(),
        )
        .commit()
        .await?;

    let first_rewritten_bcs_id = test_rewrite_commit_cs_id(
        &ctx,
        &repo,
        first,
        HashMap::new(),
        multi_mover.clone(),
        None,
    )
    .await?;

    let first_rewritten_bcs = first_rewritten_bcs_id
        .load(&ctx, &repo.repo_blobstore())
        .await?;
    let changes: Vec<_> = first_rewritten_bcs
        .file_changes()
        .map(|(path, change)| (path.to_string(), change.git_lfs().unwrap()))
        .collect();
    assert_eq!(
        changes,
        vec![
            ("small/foo.php".to_string(), GitLfs::full_content()),
            ("small/large.avi".to_string(), GitLfs::full_content())
        ],
    );
    Ok(())
}
