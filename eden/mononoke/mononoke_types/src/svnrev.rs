/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::fmt;
use std::fmt::Display;
use std::str;
use std::str::FromStr;

use anyhow::Error;
use anyhow::Result;
use anyhow::bail;
use sql::mysql;

use crate::BonsaiChangeset;

// Changeset svnrev. Present only in some repos which were imported from SVN.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[derive(mysql::OptTryFromRowField)]
#[derive(bincode::Encode, bincode::Decode)]
pub struct Svnrev(u64);

impl Svnrev {
    #[inline]
    pub const fn new(rev: u64) -> Self {
        Self(rev)
    }

    #[inline]
    pub fn id(&self) -> u64 {
        self.0
    }

    // ex. svn:uuid/path@1234
    pub fn parse_svnrev(svnrev: &str) -> Result<u64> {
        let at_pos = svnrev
            .rfind('@')
            .ok_or_else(|| Error::msg("Wrong convert_revision value"))?;
        let result = svnrev[1 + at_pos..].parse::<u64>()?;
        Ok(result)
    }

    pub fn from_bcs(bcs: &BonsaiChangeset) -> Result<Self> {
        match bcs.hg_extra().find(|(key, _)| key == &"convert_revision") {
            Some((_, svnrev)) => {
                let svnrev = Svnrev::parse_svnrev(str::from_utf8(svnrev)?)?;
                Ok(Self::new(svnrev))
            }
            None => bail!("Bonsai cs {:?} without svnrev", bcs),
        }
    }
}

impl Display for Svnrev {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        Display::fmt(&self.0, fmt)
    }
}

impl FromStr for Svnrev {
    type Err = <u64 as FromStr>::Err;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        u64::from_str(s).map(Svnrev::new)
    }
}
