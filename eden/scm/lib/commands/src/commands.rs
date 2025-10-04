/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

macro_rules! external_commands {
    [ $( $name:ident, )* ] => {
        pub(crate) fn extend_crate_command_table(table: &mut ::clidispatch::command::CommandTable) {
            $(
            {
                use ::$name as m;
                let command_aliases = m::aliases();
                let doc = m::doc();
                let synopsis = m::synopsis();
                let enable_cas = m::enable_cas();
                ::clidispatch::command::Register::register(table, m::run, &command_aliases, &doc, synopsis.as_deref(), enable_cas);
            }
            )*
        }
    }
}

external_commands![
    // see update_commands.sh
    // [[[cog
    // import cog, glob, os
    // for path in sorted(glob.glob('commands/cmd*/TARGETS')) + sorted(glob.glob('debugcommands/cmd*/TARGETS')):
    //     name = os.path.basename(os.path.dirname(path))
    //     cog.outl(f'{name},')
    // ]]]
    cmdclone,
    cmdconfig,
    cmdconfigfile,
    cmdgoto,
    cmdroot,
    cmdstatus,
    cmdversion,
    cmdwhereami,
    cmddebugargs,
    cmddebugcas,
    cmddebugconfigtree,
    cmddebugcurrentexe,
    cmddebugdumpindexedlog,
    cmddebugdumpinternalconfig,
    cmddebugfilterid,
    cmddebugfsync,
    cmddebuggitmodules,
    cmddebughttp,
    cmddebuglfsreceive,
    cmddebuglfssend,
    cmddebugmergestate,
    cmddebugmetrics,
    cmddebugnetworkdoctor,
    cmddebugpython,
    cmddebugracyoutput,
    cmddebugrefreshconfig,
    cmddebugrevsets,
    cmddebugroots,
    cmddebugrunlog,
    cmddebugscmstore,
    cmddebugscmstorereplay,
    cmddebugsegmentgraph,
    cmddebugstore,
    cmddebugstructuredprogress,
    cmddebugtestcommand,
    cmddebugtop,
    cmddebugwait,
    cmddebugwalkdetector,
    // [[[end]]]
];

use clidispatch::command::CommandTable;

#[allow(dead_code)]
/// Return the main command table including all Rust commands.
pub fn table() -> CommandTable {
    let mut table = CommandTable::new();

    extend_crate_command_table(&mut table);

    table
}
