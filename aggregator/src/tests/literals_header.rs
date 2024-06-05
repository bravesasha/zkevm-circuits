use halo2_proofs::{
    circuit::{Layouter, SimpleFloorPlanner, Value},
    dev::{MockProver, VerifyFailure},
    halo2curves::bn256::Fr,
    plonk::{Circuit, Column, ConstraintSystem, Error, Fixed},
};
use rand;
use rand::Rng;
use std::io::Write;
use zkevm_circuits::table::RangeTable;
use crate::aggregation::decoder::tables::LiteralsHeaderTable;
use crate::aggregation::witgen::{MultiBlockProcessResult, process, init_zstd_encoder, ZstdTag, ZstdWitnessRow};

#[derive(Clone)]
struct TestLiteralsHeaderConfig {
    /// Fixed column to mark all enabled rows.
    q_enable: Column<Fixed>,
    /// Range Table for [0, 8).
    range8: RangeTable<8>,
    /// Range Table for [0, 16).
    range16: RangeTable<16>,
    /// Helper table for decoding the regenerated size from LiteralsHeader.
    literals_header_table: LiteralsHeaderTable,
}

impl TestLiteralsHeaderConfig {
    fn unusable_rows() -> usize {
        64
    }
}

#[derive(Default)]
struct TestLiteralsHeaderCircuit {
    /// Degree for the test circuit, i.e. 1 << k number of rows.
    k: u32,
    /// Compressed bytes
    compressed: Vec<u8>,
    /// Variant of possible unsound case.
    case: UnsoundCase,
}

impl Circuit<Fr> for TestLiteralsHeaderCircuit {
    type Config = TestLiteralsHeaderConfig;
    type FloorPlanner = SimpleFloorPlanner;

    fn without_witnesses(&self) -> Self {
        Self::default()
    }

    fn configure(meta: &mut ConstraintSystem<Fr>) -> Self::Config {
        let q_enable = meta.fixed_column();

        // Helper tables
        let range8 = RangeTable::construct(meta);
        let range16 = RangeTable::construct(meta);
        let literals_header_table = LiteralsHeaderTable::configure(meta, q_enable, range8, range16);

        Self::Config {
            q_enable,
            range8,
            range16,
            literals_header_table,
        }
    }

    fn synthesize(
        &self,
        config: Self::Config,
        mut layouter: impl Layouter<Fr>,
    ) -> Result<(), Error> {
        let n_enabled = (1 << self.k) - Self::Config::unusable_rows();

        let MultiBlockProcessResult {
            witness_rows,
            literal_bytes: decoded_literals,
            fse_aux_tables,
            block_info_arr,
            sequence_info_arr,
            address_table_rows: address_table_arr,
            sequence_exec_results,
        } = process(&self.compressed, Value::known(Fr::from(12345)));

        // Load auxiliary tables
        config.range8.load(&mut layouter)?;
        config.range16.load(&mut layouter)?;

        /////////////////////////////////////////
        ///// Assign LiteralHeaderTable  ////////
        /////////////////////////////////////////
        let mut literal_headers: Vec<(u64, u64, (u64, u64, u64))> = vec![]; // (block_idx, byte_offset, (byte0, byte1, byte2))
        let literal_header_rows = witness_rows
            .iter()
            .filter(|r| r.state.tag == ZstdTag::ZstdBlockLiteralsHeader)
            .cloned()
            .collect::<Vec<ZstdWitnessRow<Fr>>>();
        let max_block_idx = witness_rows
            .iter()
            .last()
            .expect("Last row of witness exists.")
            .state
            .block_idx;
        for curr_block_idx in 1..=max_block_idx {
            let byte_idx = literal_header_rows
                .iter()
                .find(|r| r.state.block_idx == curr_block_idx)
                .unwrap()
                .encoded_data
                .byte_idx;

            let literal_bytes = literal_header_rows
                .iter()
                .filter(|&r| r.state.block_idx == curr_block_idx)
                .map(|r| r.encoded_data.value_byte as u64)
                .collect::<Vec<u64>>();

            literal_headers.push((
                curr_block_idx,
                byte_idx,
                (
                    literal_bytes[0],
                    if literal_bytes.len() > 1 {
                        literal_bytes[1]
                    } else {
                        0
                    },
                    if literal_bytes.len() > 2 {
                        literal_bytes[2]
                    } else {
                        0
                    },
                ),
            ));
        }

        let (assigned_literals_header_table_rows, assigned_padding_cells) = config
            .literals_header_table
            .assign(&mut layouter, literal_headers, n_enabled)?;

        // Modify assigned witness values
        let mut first_pass = halo2_base::SKIP_FIRST_PASS;
        layouter.assign_region(
            || "TestLiteralsHeaderCircuit: potentially unsound assignments",
            |mut region| {
                if first_pass {
                    first_pass = false;
                    return Ok(());
                }

                for offset in 0..n_enabled {
                    region.assign_fixed(
                        || "q_enable",
                        config.q_enable,
                        offset,
                        || Value::known(Fr::one()),
                    )?;
                }

                let mut rng = rand::thread_rng();

                match self.case {
                    // sound case
                    UnsoundCase::None => {},

                    // First block index is not 1
                    UnsoundCase::IncorrectInitialBlockIdx => {
                        let block_idx_cell =
                            assigned_literals_header_table_rows[0].clone().block_idx.expect("cell is assigned").cell();
                        let _modified_cell = region.assign_advice(
                            || "Change the first block index value",
                            block_idx_cell.column.try_into().expect("assigned cell col is valid"),
                            block_idx_cell.row_offset,
                            || Value::known(Fr::from(2)),
                        )?;
                    },

                    // Block index should increment by 1 with each valid row
                    UnsoundCase::IncorrectBlockIdxTransition => {
                        let row_idx: usize =
                            rng.gen_range(0..assigned_literals_header_table_rows.len());
                        let block_idx_cell = assigned_literals_header_table_rows[row_idx]
                            .clone()
                            .block_idx
                            .expect("cell is assigned");
                        let _modified_cell = region.assign_advice(
                            || "Corrupt the block index value at a random location",
                            block_idx_cell
                                .cell()
                                .column
                                .try_into()
                                .expect("assigned cell col is valid"),
                            block_idx_cell.cell().row_offset,
                            || block_idx_cell.value() + Value::known(Fr::one()),
                        )?;
                    },

                    // Padding indicator transitions from 1 -> 0
                    UnsoundCase::IrregularPaddingTransition => {
                        let row_idx: usize = rng.gen_range(0..assigned_padding_cells.len());
                        let is_padding_cell = assigned_padding_cells[row_idx].clone();

                        let _modified_cell = region.assign_advice(
                            || "Flip is_padding value in the padding section",
                            is_padding_cell
                                .cell()
                                .column
                                .try_into()
                                .expect("assigned cell col is valid"),
                            is_padding_cell.cell().row_offset,
                            || Value::known(Fr::zero()),
                        )?;
                    },

                    // Regen size is not calculated correctly
                    UnsoundCase::IncorrectRegenSize => {
                        let row_idx: usize =
                            rng.gen_range(0..assigned_literals_header_table_rows.len());
                        let regen_size_cell = assigned_literals_header_table_rows[row_idx]
                            .clone()
                            .regen_size
                            .expect("cell is assigned");

                        let _modified_cell = region.assign_advice(
                            || "Invalidate the regen_size value at a random location",
                            regen_size_cell
                                .cell()
                                .column
                                .try_into()
                                .expect("assigned cell col is valid"),
                            regen_size_cell.cell().row_offset,
                            || regen_size_cell.value() + Value::known(Fr::one()),
                        )?;
                    },
                }

                Ok(())
            },
        )
    }
}

enum UnsoundCase {
    /// sound case.
    None,
    /// First block index is not 1
    IncorrectInitialBlockIdx,
    /// Block index should increment by 1 with each valid row
    IncorrectBlockIdxTransition,
    /// Padding indicator transitions from 1 -> 0
    IrregularPaddingTransition,
    /// Regen size is not calculated correctly
    IncorrectRegenSize,
}

impl Default for UnsoundCase {
    fn default() -> Self {
        Self::None
    }
}

fn run(case: UnsoundCase) -> Result<(), Vec<VerifyFailure>> {
    let k = 18;

    // Batch 127
    let raw = hex::decode("00000073f8718302d9848422551000827b0c94f565295eddcc0682bb16376c742e9bc9dbb32512880429d069189e01fd8083104ec3a02b10f9f3bbaa927b805b9b225f04d90a9994da49f309fb1e029312c661ffb68ea065de06a6d34dadf1af4f80d9133a67cf7753c925f5bfd785f56c20c11280ede0000000aef8ac10841c9c38008305d0a594ec53c830f4444a8a56455c6836b5d2aa794289aa80b844f2b9fdb8000000000000000000000000b6966083c7b68175b4bf77511608aee9a80d2ca4000000000000000000000000000000000000000000000000003d83508c36cdb583104ec4a0203dff6f72962bb8aa5a9bc365c705818ad2ae51485a8c831e453668d4b75d1fa03de15a7b705a8ad59f8437b4ca717f1e8094c77c5459ee57b0cae8b6c4ebdf5e000002d7f902d402841c9c38008302c4589480e38291e06339d10aab483c65695d004dbd5c69870334ae29914c90b902642cc4081e000000000000000000000000000000000000000000000000000000000000006000000000000000000000000000000000000000000000000000000000001e9dd10000000000000000000000000000000000000000000000000000000065b3f7550000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000600000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000334ae29914c9000000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000020000000000000000000000000814a23b053fd0f102aeeda0459215c2444799c700000000000000000000000000000000000000000000000000000000000000080000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000000060000000000000000000000000530000000000000000000000000000000000000400000000000000000000000097af7be0b94399f9dd54a984e8498ce38356f0380000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000083104ec3a039a8970ba5ef7fb1cfe8d5db8e293b9837e298fd55faab853f56b66322c8ef80a0523ceb389a544389f6775b8ff982feac3f05b092869e35fed509e828d5e5759900000170f9016d03841c9c38008302a98f9418b71386418a9fca5ae7165e31c385a5130011b680b9010418cbafe5000000000000000000000000000000000000000000000000000000000091855b000000000000000000000000000000000000000000000000000e9f352b7fc38100000000000000000000000000000000000000000000000000000000000000a00000000000000000000000006fd71e5088bdaaed42efd384fede02a76dca87f00000000000000000000000000000000000000000000000000000000065b3cd55000000000000000000000000000000000000000000000000000000000000000200000000000000000000000006efdbff2a14a7c8e15944d1f4a48f9f95f663a4000000000000000000000000530000000000000000000000000000000000000483104ec4a097411c6aad88135b8918b29d318898808fc04e933379c6d5da9f267315af2300a0265e6b38e475244e2639ea6eb42ae0d7f4f227a9dd9dd5328744bcd9f5f25bd2000000aff8ad82077a841c9c3800826716940a88bc5c32b684d467b43c06d9e0899efeaf59df86d12f0c4c832ab83e646174613a2c7b2270223a226c61796572322d3230222c226f70223a22636c61696d222c227469636b223a22244c32222c22616d74223a2231303030227d83104ec3a0bef600f17b5037519044f2296d1181abf140b986a7d43b7472e93b6be8378848a03c951790ad335a2d1947b3913e2280e506b17463c5f3b61849595a94b37439b3").expect("Decoding hex data should not fail");

    let compressed = {
        // compression level = 0 defaults to using level=3, which is zstd's default.
        let mut encoder = init_zstd_encoder(None);

        // set source length, which will be reflected in the frame header.
        encoder
            .set_pledged_src_size(Some(raw.len() as u64))
            .expect("Encoder src_size: raw.len()");
        // include the content size to know at decode time the expected size of decoded data.

        encoder.write_all(&raw).expect("Encoder wirte_all");
        encoder.finish().expect("Encoder success")
    };

    let test_circuit = TestLiteralsHeaderCircuit {
        k,
        compressed,
        case,
    };

    let prover =
        MockProver::run(k, &test_circuit, vec![]).expect("unexpected failure: MockProver::run");
    prover.verify_par()
}

#[test]
fn test_literals_header_ok() {
    assert!(run(UnsoundCase::None).is_ok())
}

#[test]
fn test_incorrect_initial_block_idx() {
    assert!(run(UnsoundCase::IncorrectInitialBlockIdx).is_err())
}

#[test]
fn test_incorrect_block_idx_transition() {
    assert!(run(UnsoundCase::IncorrectBlockIdxTransition).is_err())
}

#[test]
fn test_irregular_padding_transition() {
    assert!(run(UnsoundCase::IrregularPaddingTransition).is_err())
}

#[test]
fn test_incorrect_regen_size() {
    assert!(run(UnsoundCase::IncorrectRegenSize).is_err())
}