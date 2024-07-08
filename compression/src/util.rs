use aggregator_snark_verifier::{
    halo2_base::AssignedValue,
    halo2_ecc::bigint::ProperCrtUint,
    loader::{
        halo2::{EcPoint, Halo2Loader},
        native::NativeLoader,
    },
    pcs::{
        kzg::{Bdfg21, KzgAccumulator, KzgAs},
        AccumulationSchemeProver,
    },
    util::arithmetic::fe_to_limbs,
    verifier::SnarkVerifier,
    Error as SnarkVerifierError,
};
use aggregator_snark_verifier_sdk::{
    halo2::{aggregation::BaseFieldEccChip, PoseidonTranscript, POSEIDON_SPEC},
    PlonkSuccinctVerifier, Snark, BITS, LIMBS, SHPLONK,
};
use halo2_proofs::poly::{commitment::ParamsProver, kzg::commitment::ParamsKZG};
use halo2curves::{
    bn256::{Bn256, Fq, Fr, G1Affine, G2Affine},
    pairing::Engine,
};
use rand::Rng;
use std::rc::Rc;

/// Subroutine for the witness generations.
/// Extract proof from previous snarks and check pairing for accumulation.
pub fn extract_proof_and_instances_with_pairing_check(
    params: &ParamsKZG<Bn256>,
    snarks: &[Snark],
    rng: impl Rng + Send,
) -> Result<(Vec<u8>, Vec<Fr>), SnarkVerifierError> {
    // (old_accumulator, public inputs) -> (new_accumulator, public inputs)
    let (accumulator, as_proof) =
        extract_accumulators_and_proof(params, snarks, rng, &params.g2(), &params.s_g2())?;

    // the instance for the outer circuit is
    // - new accumulator, consists of 12 elements
    // - inner circuit's instance, flattened (old accumulator is stripped out if exists)
    //
    // it is important that new accumulator is the first 12 elements
    // as specified in CircuitExt::accumulator_indices()
    let KzgAccumulator::<G1Affine, NativeLoader> { lhs, rhs } = accumulator;

    // sanity check on the accumulator
    {
        let left = Bn256::pairing(&lhs, &params.g2());
        let right = Bn256::pairing(&rhs, &params.s_g2());
        log::trace!("circuit acc check: left {:?}", left);
        log::trace!("circuit acc check: right {:?}", right);

        if left != right {
            return Err(SnarkVerifierError::AssertionFailure(format!(
                "accumulator check failed {left:?} {right:?}",
            )));
        }
    }

    let acc_instances = [lhs.x, lhs.y, rhs.x, rhs.y]
        .map(fe_to_limbs::<Fq, Fr, { LIMBS }, { BITS }>)
        .concat();

    Ok((as_proof, acc_instances))
}

pub fn flatten_accumulator(
    accumulator: KzgAccumulator<G1Affine, Rc<Halo2Loader<G1Affine, BaseFieldEccChip>>>,
) -> Vec<AssignedValue<Fr>> {
    let KzgAccumulator { lhs, rhs } = accumulator;
    let [lhs_assigned, rhs_assigned] = [lhs, rhs].map(EcPoint::into_assigned);
    [
        lhs_assigned.x,
        lhs_assigned.y,
        rhs_assigned.x,
        rhs_assigned.y,
    ]
    .iter()
    .flat_map(ProperCrtUint::limbs)
    .cloned()
    .collect()
}

fn extract_accumulators_and_proof(
    params: &ParamsKZG<Bn256>,
    snarks: &[Snark],
    rng: impl Rng + Send,
    g2: &G2Affine,
    s_g2: &G2Affine,
) -> Result<(KzgAccumulator<G1Affine, NativeLoader>, Vec<u8>), SnarkVerifierError> {
    let svk = params.get_g()[0].into();

    let mut transcript_read =
        PoseidonTranscript::<NativeLoader, &[u8]>::from_spec(&[], POSEIDON_SPEC.clone());
    let accumulators: Vec<KzgAccumulator<_, _>> = snarks
        .iter()
        .flat_map(|snark| {
            transcript_read.new_stream(snark.proof.as_slice());
            let proof = PlonkSuccinctVerifier::<SHPLONK>::read_proof(
                &svk,
                &snark.protocol,
                &snark.instances,
                &mut transcript_read,
            )
            .unwrap();
            // each accumulator has (lhs, rhs) based on Shplonk
            // lhs and rhs are EC points
            let x: Vec<KzgAccumulator<_, _>> = PlonkSuccinctVerifier::<SHPLONK>::verify(
                &svk,
                &snark.protocol,
                &snark.instances,
                &proof,
            )
            .unwrap();
            x
        })
        .collect::<Vec<_>>();

    // sanity check on the accumulator
    {
        for (i, acc) in accumulators.iter().enumerate() {
            let KzgAccumulator { lhs, rhs } = acc;
            let left = Bn256::pairing(lhs, g2);
            let right = Bn256::pairing(rhs, s_g2);
            log::trace!("acc extraction {}-th acc check: left {:?}", i, left);
            log::trace!("acc extraction {}-th acc check: right {:?}", i, right);
            if left != right {
                return Err(SnarkVerifierError::AssertionFailure(format!(
                    "accumulator check failed {left:?} {right:?}, index {i}",
                )));
            }
            //assert_eq!(left, right, "accumulator check failed");
        }
    }

    let mut transcript_write =
        PoseidonTranscript::<NativeLoader, Vec<u8>>::from_spec(vec![], POSEIDON_SPEC.clone());
    // We always use SHPLONK for accumulation scheme when aggregating proofs
    let accumulator =
        // core step
        // KzgAs does KZG accumulation scheme based on given accumulators and random number (for adding blinding)
        // accumulated ec_pt = ec_pt_1 * 1 + ec_pt_2 * r + ... + ec_pt_n * r^{n-1}
        // ec_pt can be lhs and rhs
        // r is the challenge squeezed from proof
        KzgAs::<Bn256, Bdfg21>::create_proof::<PoseidonTranscript<NativeLoader, Vec<u8>>, _>(
            &Default::default(),
            &accumulators,
            &mut transcript_write,
            rng,
        )?;
    Ok((accumulator, transcript_write.finalize()))
}
