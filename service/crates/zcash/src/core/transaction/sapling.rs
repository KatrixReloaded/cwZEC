//! No Sapling builder is configured. Fail closed if upstream ever invokes a prover.
use rand_core::Rng;
use sapling_crypto::{
    Diversifier, MerklePath, PaymentAddress, ProofGenerationKey, Rseed, circuit,
    prover::{OutputProver, SpendProver},
    value::{NoteValue, ValueCommitTrapdoor},
};
use std::convert::Infallible;

pub(super) struct DisabledSapling;

impl SpendProver for DisabledSapling {
    type Proof = Infallible;
    fn prepare_circuit(
        _: ProofGenerationKey,
        _: Diversifier,
        _: Rseed,
        _: NoteValue,
        _: jubjub::Fr,
        _: ValueCommitTrapdoor,
        _: bls12_381::Scalar,
        _: MerklePath,
    ) -> Option<circuit::Spend> {
        None
    }
    fn create_proof<R: Rng>(&self, _: circuit::Spend, _: &mut R) -> Infallible {
        panic!("Sapling spending is disabled")
    }
    fn encode_proof(proof: Infallible) -> sapling_crypto::bundle::GrothProofBytes {
        match proof {}
    }
}

impl OutputProver for DisabledSapling {
    type Proof = Infallible;
    fn prepare_circuit(
        _: &sapling_crypto::keys::EphemeralSecretKey,
        _: PaymentAddress,
        _: jubjub::Fr,
        _: NoteValue,
        _: ValueCommitTrapdoor,
    ) -> circuit::Output {
        panic!("Sapling outputs are disabled")
    }
    fn create_proof<R: Rng>(&self, _: circuit::Output, _: &mut R) -> Infallible {
        panic!("Sapling proving is disabled")
    }
    fn encode_proof(proof: Infallible) -> sapling_crypto::bundle::GrothProofBytes {
        match proof {}
    }
}
