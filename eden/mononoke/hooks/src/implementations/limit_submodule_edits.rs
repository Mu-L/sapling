/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::Error;
use anyhow::Result;
use async_trait::async_trait;
use bookmarks::BookmarkKey;
use context::CoreContext;
use mononoke_types::BonsaiChangeset;
use mononoke_types::FileChange;
use mononoke_types::FileType;
use mononoke_types::NonRootMPath;
use regex::Regex;
use serde::Deserialize;

use crate::ChangesetHook;
use crate::CrossRepoPushSource;
use crate::HookConfig;
use crate::HookExecution;
use crate::HookRejectionInfo;
use crate::HookRepo;
use crate::PushAuthoredBy;

const NAMED_CAPTURE_NAME: &str = "marker_capture";

#[derive(Clone, Debug, Deserialize)]
pub struct LimitSubmoduleEditsConfig {
    allow_edits_with_marker: Option<String>,
}
#[derive(Clone, Debug)]
struct ChangesAllowedWithMarkerOptions {
    marker_extraction_regex: Regex,
    marker: String,
}

#[derive(Clone, Debug)]
pub struct LimitSubmoduleEditsHook {
    changes_allowed_with_marker_options: Option<ChangesAllowedWithMarkerOptions>,
}

impl LimitSubmoduleEditsHook {
    pub fn new(config: &HookConfig) -> Result<Self> {
        Self::with_config(config.parse_options()?)
    }

    pub fn with_config(config: LimitSubmoduleEditsConfig) -> Result<Self> {
        let changes_allowed_with_marker_options =
            if let Some(marker) = config.allow_edits_with_marker {
                let marker_extraction_regex = Regex::new(&format!(
                    r"{}:\s*(?<{}>.+?)($|\n|\s)",
                    &marker, &NAMED_CAPTURE_NAME
                ))?;
                Some(ChangesAllowedWithMarkerOptions {
                    marker_extraction_regex,
                    marker,
                })
            } else {
                None
            };

        Ok(Self {
            changes_allowed_with_marker_options,
        })
    }
}

fn get_submodule_mpath(changeset: &BonsaiChangeset) -> Option<&NonRootMPath> {
    for (mpath, fc) in changeset.file_changes() {
        if let FileChange::Change(tfc) = fc {
            if tfc.file_type() == FileType::GitSubmodule {
                return Some(mpath);
            }
        }
    }
    None
}

fn extract_path_from_marker<'a>(
    options: &'a ChangesAllowedWithMarkerOptions,
    changeset: &'a BonsaiChangeset,
) -> Option<&'a str> {
    let captures = options
        .marker_extraction_regex
        .captures(changeset.message())?;
    Some(captures.name(NAMED_CAPTURE_NAME)?.as_str())
}

#[async_trait]
impl ChangesetHook for LimitSubmoduleEditsHook {
    async fn run<'this: 'cs, 'ctx: 'this, 'cs, 'repo: 'cs>(
        &'this self,
        _ctx: &'ctx CoreContext,
        _hook_repo: &'repo HookRepo,
        _bookmark: &BookmarkKey,
        changeset: &'cs BonsaiChangeset,
        _cross_repo_push_source: CrossRepoPushSource,
        _push_authored_by: PushAuthoredBy,
    ) -> Result<HookExecution, Error> {
        match (
            &self.changes_allowed_with_marker_options,
            get_submodule_mpath(changeset),
        ) {
            (_, None) => Ok(HookExecution::Accepted),
            (None, Some(submodule_path)) => {
                Ok(HookExecution::Rejected(HookRejectionInfo::new_long(
                    "Git submodules or any changes to them are not allowed in this repository.",
                    format!(
                        "Commit creates or edits a submodule at path {}",
                        submodule_path
                    ),
                )))
            }
            (Some(options), Some(submodule_path)) => {
                match extract_path_from_marker(options, changeset) {
                    Some(path_from_marked_commit) => {
                        if path_from_marked_commit == submodule_path.to_string() {
                            Ok(HookExecution::Accepted)
                        } else {
                            Ok(HookExecution::Rejected(HookRejectionInfo::new_long(
                                "Changes to git submodules are restricted in this repository.",
                                format!(
                                    "Commit creates or edits a submodule at path {}. The content of the \"{}\" marker, do not match the path of the submodule: \"{}\" != \"{}\"",
                                    submodule_path,
                                    options.marker,
                                    path_from_marked_commit,
                                    submodule_path,
                                ),
                            )))
                        }
                    }
                    None => Ok(HookExecution::Rejected(HookRejectionInfo::new_long(
                        "Changes to git submodules are restricted in this repository.",
                        format!(
                            "Commit creates or edits a submodule at path {}. If you did mean to do this, add \"{}: {}\" to your commit message",
                            submodule_path, options.marker, submodule_path,
                        ),
                    ))),
                }
            }
        }
    }
}
