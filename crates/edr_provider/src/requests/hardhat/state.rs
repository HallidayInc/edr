use edr_primitives::{Address, Bytes, U256};

use crate::{
    data::ProviderData, spec::SyncProviderSpec, time::TimeSinceEpoch, ProviderErrorForChainSpec,
};

pub fn handle_set_balance<ChainSpecT: SyncProviderSpec<TimerT>, TimerT: Clone + TimeSinceEpoch>(
    data: &mut ProviderData<ChainSpecT, TimerT>,
    address: Address,
    balance: U256,
) -> Result<bool, ProviderErrorForChainSpec<ChainSpecT>> {
    // on chains with a native token mirror, the gas token's user-facing decimals
    // are the mirror token's decimals; interpret the requested balance there and
    // translate to the underlying 18-decimal native representation. add a small
    // native gas headroom so paying tx fees does not drop the apparent (mirror)
    // balance below the requested amount.
    let native = match data.native_token_mirror() {
        Some(mirror) => mirror
            .erc20_to_native_balance(balance)
            .saturating_add(U256::from(10u64).pow(U256::from(18))),
        None => balance,
    };
    data.set_balance(address, native)?;
    Ok(true)
}

pub fn handle_set_code<ChainSpecT: SyncProviderSpec<TimerT>, TimerT: Clone + TimeSinceEpoch>(
    data: &mut ProviderData<ChainSpecT, TimerT>,
    address: Address,
    code: Bytes,
) -> Result<bool, ProviderErrorForChainSpec<ChainSpecT>> {
    data.set_code(address, code)?;

    Ok(true)
}

pub fn handle_set_nonce<ChainSpecT: SyncProviderSpec<TimerT>, TimerT: Clone + TimeSinceEpoch>(
    data: &mut ProviderData<ChainSpecT, TimerT>,
    address: Address,
    nonce: u64,
) -> Result<bool, ProviderErrorForChainSpec<ChainSpecT>> {
    data.set_nonce(address, nonce)?;

    Ok(true)
}

pub fn handle_set_storage_at<
    ChainSpecT: SyncProviderSpec<TimerT>,
    TimerT: Clone + TimeSinceEpoch,
>(
    data: &mut ProviderData<ChainSpecT, TimerT>,
    address: Address,
    index: U256,
    value: U256,
) -> Result<bool, ProviderErrorForChainSpec<ChainSpecT>> {
    // if this address is a native-token mirror, look up the slot in the
    // persistent keccak cache (populated by interpreter KECCAK256 observations
    // during prior balanceOf/transfer calls). on a match, route the write
    // through set_balance so the underlying native balance becomes the single
    // source of truth (otherwise SLOAD via the interpreter would return native
    // — which we never updated — instead of the value we just wrote).
    if let Some(mirror) = data.native_token_mirror()
        && address == mirror.token
    {
        let owner = edr_mirror::current_cache()
            .lock()
            .unwrap()
            .get(&index)
            .copied();
        if let Some(owner) = owner {
            return handle_set_balance(data, owner, value);
        }
    }
    data.set_account_storage_slot(address, index, value)?;
    Ok(true)
}
