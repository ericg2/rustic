//! `restore` subcommand

use crate::{
    Application, RUSTIC_APP, helpers::bytes_size_to_string, repository::IndexedRepo, status_err,
};
use std::collections::BTreeMap;
use std::path::Path;

use crate::filtering::SnapshotFilter;
use abscissa_core::{Command, Runnable, Shutdown};
use anyhow::{anyhow, Result};
use conflate::Merge;
use log::{debug, info};
use rustic_backend::local::LocalDestination;
use rustic_backend::opendal::{OpenDALConfig, OpenDALDestination};
use rustic_core::{DestinationBuilder, LsOptions, RestoreOptions};
use serde::{Deserialize, Serialize};
//use crate::helpers::up_level;

/// `restore` subcommand
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Command, Default, Debug, clap::Parser, Serialize, Deserialize, Merge)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct RestoreCmd {
    /// Snapshot/path to restore
    ///
    /// Snapshot can be identified the following ways: "01a2b3c4" or "latest" or "latest~N" (N >= 0)
    #[clap(value_name = "SNAPSHOT[:PATH]")]
    #[serde(skip)]
    #[merge(skip)]
    snap: String,

    /// Restore destination
    #[clap(value_name = "DESTINATION")]
    #[merge(strategy=conflate::option::overwrite_none)]
    dest: Option<String>,

    /// Restore options
    #[clap(flatten)]
    #[serde(skip)]
    #[merge(skip)]
    opts: RestoreOptions,

    /// Other options for the destination.
    #[clap(skip)]
    #[merge(strategy = conflate::btreemap::append_or_ignore)]
    options: BTreeMap<String, String>,

    /// List options
    #[clap(flatten)]
    #[serde(skip)]
    #[merge(skip)]
    ls_opts: LsOptions,

    /// Snapshot filter options (when using latest)
    #[clap(
        flatten,
        next_help_heading = "Snapshot filter options (when using latest)"
    )]
    #[merge(skip)]
    #[serde(skip)]
    filter: SnapshotFilter,
}
impl Runnable for RestoreCmd {
    fn run(&self) {
        if let Err(err) = RUSTIC_APP
            .config()
            .repository
            .run_indexed(|repo| self.clone().inner_run(repo))
        {
            status_err!("{}", err);
            RUSTIC_APP.shutdown(Shutdown::Crash);
        };
    }
}

impl RestoreCmd {
    fn restore(&self, repo: IndexedRepo, dest: impl DestinationBuilder) -> Result<()> {
        let config = RUSTIC_APP.config();
        let dry_run = config.global.dry_run;

        let node =
            repo.node_from_snapshot_path(&self.snap, |sn| config.snapshot_filter.matches(sn))?;

        // for restore, always recurse into tree
        let mut ls_opts = self.ls_opts.clone();
        ls_opts.recursive = true;
        let ls = repo.ls(&node, &ls_opts)?;
        let restore_infos = repo.prepare_restore(&self.opts, ls, &dest, dry_run)?;

        let fs = restore_infos.stats.files;
        println!(
            "Files:  {} to restore, {} unchanged, {} verified, {} to modify, {} additional",
            fs.restore, fs.unchanged, fs.verified, fs.modify, fs.additional
        );
        let ds = restore_infos.stats.dirs;
        println!(
            "Dirs:   {} to restore, {} to modify, {} additional",
            ds.restore, ds.modify, ds.additional
        );

        info!(
            "total restore size: {}",
            bytes_size_to_string(restore_infos.restore_size)
        );
        if restore_infos.matched_size > 0 {
            info!(
                "using {} of existing file contents.",
                bytes_size_to_string(restore_infos.matched_size)
            );
        }
        if restore_infos.restore_size == 0 {
            info!("all file contents are fine.");
        }

        if dry_run && config.global.dry_run_warmup {
            repo.warm_up(restore_infos.to_packs().into_iter())?;
        } else if !dry_run && !config.global.dry_run_warmup {
            // save some memory
            let repo = repo.drop_data_from_index();

            let ls = repo.ls(&node, &ls_opts)?;
            repo.restore(restore_infos, &self.opts, ls, &dest)?;
            println!("restore done.");
        } else {
            debug!(
                "--dry-run is without warmup, --dry-run --dry-run-warmup also issues the warmup script."
            );
        }

        Ok(())
    }

    fn inner_run(mut self, repo: IndexedRepo) -> Result<()> {
        // TODO: This check should preferably not be hardcoded.
        let config = RUSTIC_APP.config();
        self.merge(config.restore.clone());

        let dest = self.dest.clone().ok_or_else(|| anyhow!("A valid destination is required."))?;
        if let Some(scheme) = dest.strip_prefix("opendal:") {
            let config = OpenDALConfig::from_iter(scheme, self.options.clone());
            let dest = OpenDALDestination::new(Path::new("/"), &config);
            self.restore(repo, dest)
        } else {
            let dest = LocalDestination::new(&dest);
            self.restore(repo, dest)
        }
    }
}
