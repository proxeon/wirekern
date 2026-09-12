//! WhatsApp Cloud CLI: typed sends, configure, signed webhook parse.

mod cmd;
mod dispatch;
mod helpers;

pub(crate) use cmd::*;
pub(crate) use dispatch::{configure, dispatch, send_allowed};
#[cfg(test)]
pub(crate) use helpers::*;
