//! Concrete bounded CWZEC transaction client, opaque state reads, and permit signing.
pub mod bindings;
mod checkpoint;
mod deployment;
pub use deployment::DeploymentEvidence;
pub mod client;
#[cfg(test)]
mod deployment_tests;
mod signer;
pub use signer::load_signer;
pub mod permit;
pub use client::*;

#[cfg(test)]
mod client_tests;
