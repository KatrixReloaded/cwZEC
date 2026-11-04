//! Compile-only verification of the direct NU7 signing entrypoint.
//! No parameters, spending keys or transaction are created by this probe.

use orchard::keys::SpendAuthorizingKey;
use rand_core::{CryptoRng, Rng};
use sapling_crypto::prover::{OutputProver, SpendProver};
use zcash_primitives::transaction::{
    builder::{BuildResult, Builder, Error},
    fees::FeeRule,
};
use zcash_protocol::consensus::TestNetwork;
use zcash_transparent::builder::TransparentSigningSet;

#[allow(dead_code, clippy::too_many_arguments)]
fn direct_signing_api<R: Rng + CryptoRng, SP: SpendProver, OP: OutputProver, FR: FeeRule>(
    builder: Builder<TestNetwork, ()>,
    keys: &[SpendAuthorizingKey],
    rng: R,
    spend_prover: &SP,
    output_prover: &OP,
    fee_rule: &FR,
) -> Result<BuildResult, Error<FR::Error>> {
    builder.build(
        &TransparentSigningSet::new(),
        &[],
        keys,
        rng,
        spend_prover,
        output_prover,
        fee_rule,
    )
}
