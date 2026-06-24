// command line interface. global flags override the config file (see config.rs
// for precedence). flags are marked global so they may appear before or after
// the subcommand.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "raiden",
    version,
    about = "provision and maintain full-disk-encrypted RAID systems on Debian"
)]
pub struct Cli {
    #[command(flatten)]
    pub global: Global,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Args)]
pub struct Global {
    /// path to the config file
    #[arg(long, default_value = "raiden.toml", global = true)]
    pub config: PathBuf,
    /// print the commands that would run without executing them
    #[arg(long, global = true)]
    pub dry_run: bool,
    /// skip the destructive-operation confirmation
    #[arg(long, global = true)]
    pub yes: bool,
    /// resume an interrupted operation from its last checkpoint
    #[arg(long, global = true)]
    pub resume: bool,
    /// read the encryption password from this file instead of prompting
    #[arg(long, global = true)]
    pub password_file: Option<PathBuf>,
    /// verbose output
    #[arg(short, long, global = true)]
    pub verbose: bool,
    /// override raid.stack
    #[arg(long, global = true)]
    pub stack: Option<String>,
    /// override raid.level
    #[arg(long, global = true)]
    pub level: Option<String>,
    /// override disks.members (comma-separated)
    #[arg(long, global = true)]
    pub members: Option<String>,
    /// override install.release
    #[arg(long, global = true)]
    pub release: Option<String>,
}

#[derive(Subcommand)]
pub enum Command {
    /// generate a starter raiden.toml for this machine
    Init(InitArgs),
    /// run the full install pipeline
    Install(InstallArgs),
    /// rebuild specific disks (all layers by default, or only those named)
    Replace {
        /// comma-separated disks to rebuild
        #[arg(long)]
        disks: String,
        /// rebuild the esp (p1) only [default: all layers]
        #[arg(long)]
        esp: bool,
        /// rebuild /boot (p2) only
        #[arg(long)]
        boot: bool,
        /// rebuild the root member (p3) only -- includes the array resilver
        #[arg(long)]
        root: bool,
    },
    /// array health and read-error mapping
    Status(StatusArgs),
    /// start or check a scrub
    Scrub {
        /// wait for the scrub to finish
        #[arg(long)]
        wait: bool,
    },
    /// assemble, unlock, and mount from a livecd
    Rescue,
    /// ensure the stack is open and mounted (or just /boot + /boot/efi with --boot)
    Mount(MountArgs),
    /// unmount, stop arrays, lock crypt
    Close,
    /// detach disks from the array
    Remove {
        /// comma-separated disks to remove
        #[arg(long)]
        disks: String,
    },
    /// run the fsync-bound fileio benchmark on the array
    Benchmark(BenchmarkArgs),
    /// inspect configuration
    #[command(subcommand)]
    Config(ConfigCmd),
    /// list candidate disks and array members
    Devices,
}

#[derive(Args)]
pub struct MountArgs {
    /// mount only /boot and /boot/efi (no crypt/array, no password)
    #[arg(long)]
    pub boot: bool,
    /// directory to mount under (default /mnt); use / for the running system
    #[arg(long, default_value = "/mnt")]
    pub at: String,
}

#[derive(Args)]
pub struct StatusArgs {
    /// show only the files affected by unrecoverable read errors (md stacks)
    #[arg(long)]
    pub bad_files: bool,
}

#[derive(Args)]
pub struct BenchmarkArgs {
    /// working-set size (sysbench --file-total-size), eg. 2G
    #[arg(long)]
    pub size: Option<String>,
    /// passes per write mode
    #[arg(long)]
    pub passes: Option<u32>,
    /// random-write events per pass
    #[arg(long)]
    pub rndwr_events: Option<u64>,
    /// sequential-write events per pass
    #[arg(long)]
    pub seqwr_events: Option<u64>,
    /// output format: "text" (default) or "json"
    #[arg(long, default_value = "text")]
    pub format: String,
}

#[derive(Args)]
pub struct InitArgs {
    /// where to write the config (default: the --config path)
    #[arg(long)]
    pub output: Option<PathBuf>,
    /// overwrite an existing config file
    #[arg(long)]
    pub force: bool,
    /// boot mode: "efi" or "bios" (default: detected from /sys/firmware/efi)
    #[arg(long)]
    pub boot_mode: Option<String>,
    /// accept the detected defaults without prompting
    #[arg(long)]
    pub non_interactive: bool,
}

#[derive(Args)]
pub struct InstallArgs {
    /// resume from this phase
    #[arg(long)]
    pub from: Option<String>,
    /// run only this phase
    #[arg(long)]
    pub only: Option<String>,
    /// list the install phases and exit
    #[arg(long)]
    pub list_phases: bool,
}

#[derive(Subcommand)]
pub enum ConfigCmd {
    /// print the resolved config and derived layout
    Show,
    /// validate the config without touching disks
    Validate,
}
