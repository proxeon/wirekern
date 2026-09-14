//! Paused advertising-management vocabulary.
//!
//! These types deliberately describe only drafts that cannot deliver. They
//! stay outside the Meta connector so a future ads connector can reuse the
//! Client/Policy seam while mapping its own wire format.

mod create;
mod creatives;
mod helpers;
mod inventory;
mod lifecycle;
mod objective;
mod review;
mod targeting;

#[cfg(test)]
mod tests;

pub use create::*;
pub use creatives::*;
pub use inventory::*;
pub use lifecycle::*;
pub use objective::*;
pub use review::*;
pub use targeting::*;
