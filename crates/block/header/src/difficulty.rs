use edr_chain_spec::EvmSpecId;
use edr_primitives::{KECCAK_RLP_EMPTY_ARRAY, U256};

use crate::BlockHeader;

fn bomb_delay(spec_id: EvmSpecId, block_number: u64) -> u64 {
    match spec_id {
        EvmSpecId::FRONTIER
        | EvmSpecId::HOMESTEAD
        | EvmSpecId::TANGERINE
        | EvmSpecId::SPURIOUS_DRAGON => 0,
        EvmSpecId::BYZANTIUM => 3000000,
        EvmSpecId::PETERSBURG => 5000000,
        // Muir Glacier didn't change EVM execution, so newer REVM versions no
        // longer have a distinct spec id for it.
        EvmSpecId::ISTANBUL if block_number >= 9_200_000 => 9_000_000,
        EvmSpecId::ISTANBUL => 5_000_000,
        EvmSpecId::BERLIN => 9_000_000,
        // Arrow and Gray Glacier were also header-only hardforks. Preserve
        // their mainnet difficulty-bomb delays using their activation blocks.
        EvmSpecId::LONDON if block_number >= 15_050_000 => 11_400_000,
        EvmSpecId::LONDON if block_number >= 13_773_000 => 10_700_000,
        EvmSpecId::LONDON => 9_000_000,
        _ => {
            unreachable!("Post-merge hardforks don't have a bomb delay")
        }
    }
}

/// Calculates the mining difficulty of a block.
pub fn calculate_ethash_canonical_difficulty(
    spec_id: EvmSpecId,
    parent: &BlockHeader,
    block_number: u64,
    block_timestamp: u64,
    min_ethash_difficulty: u64,
) -> U256 {
    // TODO: Create a custom config that prevents usage of older hardforks
    assert!(
        spec_id >= EvmSpecId::BYZANTIUM,
        "Hardforks older than Byzantium are not supported"
    );

    let bound_divisor = U256::from(2048);
    let offset = parent.difficulty / bound_divisor;

    let mut difficulty = {
        let uncle_addend = if parent.ommers_hash == KECCAK_RLP_EMPTY_ARRAY {
            1
        } else {
            2
        };
        let a = (block_timestamp - parent.timestamp) / 9;

        if let Some(a) = a.checked_sub(uncle_addend) {
            let a = U256::from(a.min(99));

            parent.difficulty - a * offset
        } else {
            let a = U256::from(uncle_addend - a);
            parent.difficulty + a * offset
        }
    };

    if let Some(exp) = block_number
        .checked_sub(bomb_delay(spec_id, block_number))
        .and_then(|num| (num / 100000).checked_sub(2))
    {
        difficulty += U256::from(2u64).pow(U256::from(exp));
    }

    difficulty.max(U256::from(min_ethash_difficulty))
}
