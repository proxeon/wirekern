//! Top-level clap parser. Test helpers use a larger stack than the harness default.
use crate::commands::Commands;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "postkit",
    version,
    about = "Publish to official APIs. BYO credentials."
)]
pub(crate) struct Cli {
    /// JSON document on stdout (agents). Human text on stderr otherwise.
    #[arg(long, global = true)]
    pub(crate) json: bool,
    /// Vault root. Default ~/.postkit
    #[arg(long, global = true, env = "POSTKIT_HOME")]
    pub(crate) home: Option<PathBuf>,
    /// Seconds for a network operation, including any token refresh on the way. Default 30.
    #[arg(long, global = true, default_value_t = 30)]
    pub(crate) deadline: u64,
    /// Account alias. Default default.
    #[arg(long, global = true, default_value = "default")]
    pub(crate) account: String,
    #[command(subcommand)]
    // The command grammar has intentionally grown large (Ads and WhatsApp
    // carry many closed, typed subcommands). Keep the selected variant out
    // of the top-level CLI value's stack footprint.
    pub(crate) command: Box<Commands>,
}

#[cfg(test)]
impl Cli {
    /// Rust's test harness gives each worker a much smaller stack than the
    /// normal `postkit` process. Clap builds a broad command tree while
    /// parsing, so run parser assertions on an ordinary 8 MiB stack instead
    /// of making individual tests depend on `RUST_MIN_STACK` in CI.
    pub(crate) fn try_parse_from<I, T>(args: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T> + Send + 'static,
        T: Into<std::ffi::OsString> + Clone + Send + 'static,
    {
        std::thread::Builder::new()
            .name("postkit-cli-parse-test".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || <Self as Parser>::try_parse_from(args))
            .expect("start parser test worker")
            .join()
            .expect("parser test worker must not panic")
    }

    /// `CommandFactory::command` builds the same broad Clap tree as parsing.
    /// Keep help-text assertions on the same main-process-sized stack as the
    /// parser assertions above, rather than leaving one CI-only overflow.
    pub(crate) fn command() -> clap::Command {
        std::thread::Builder::new()
            .name("postkit-cli-help-test".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(<Self as clap::CommandFactory>::command)
            .expect("start help test worker")
            .join()
            .expect("help test worker must not panic")
    }
}
