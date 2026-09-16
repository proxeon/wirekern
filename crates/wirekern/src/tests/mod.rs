//! Client-kernel unit tests. Connector tests live next to each connector.

#[cfg(feature = "meta-ads")]
mod ads;
#[cfg(feature = "meta-ads")]
mod insights;
mod mock;
mod publish;
mod reads;

#[cfg(feature = "whatsapp-cloud")]
mod whatsapp;

#[cfg(feature = "draft")]
mod draft_tests;
