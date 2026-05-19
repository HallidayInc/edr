use edr_eip1559::BaseFeeParams;
use edr_eip7892::ScheduledBlobParams;
use edr_primitives::{keccak256, Address, U256};

/// Fork condition for a hardfork.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ForkCondition {
    /// Activation based on block number.
    Block(u64),
    /// Activation based on UNIX timestamp.
    Timestamp(u64),
}

/// A type representing the activation of a hardfork.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HardforkActivation<HardforkT> {
    /// The condition for the hardfork activation.
    pub condition: ForkCondition,
    /// The hardfork to be activated.
    pub hardfork: HardforkT,
}

/// A struct that stores the hardforks for a chain.
#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct HardforkActivations<HardforkT> {
    /// (Start block number -> hardfork) mapping
    hardforks: Vec<HardforkActivation<HardforkT>>,
}

impl<HardforkT> HardforkActivations<HardforkT> {
    /// Constructs a new instance with the provided hardforks.
    pub fn new(hardforks: Vec<HardforkActivation<HardforkT>>) -> Self {
        Self { hardforks }
    }

    /// Returns the inner hardforks.
    pub fn into_inner(self) -> Vec<HardforkActivation<HardforkT>> {
        self.hardforks
    }

    /// Creates a new instance for a new chain with the provided hardfork.
    pub fn with_spec_id(hardfork: HardforkT) -> Self {
        Self {
            hardforks: vec![HardforkActivation {
                condition: ForkCondition::Block(0),
                hardfork,
            }],
        }
    }

    /// Whether no hardforks activations are present.
    pub fn is_empty(&self) -> bool {
        self.hardforks.is_empty()
    }
}

impl<HardforkT: Clone> HardforkActivations<HardforkT> {
    /// Returns the hardfork's `SpecId` corresponding to the provided block
    /// number.
    pub fn hardfork_at_block(&self, block_number: u64, timestamp: u64) -> Option<HardforkT> {
        self.hardforks
            .iter()
            .rev()
            .find(|HardforkActivation { condition, .. }| match condition {
                ForkCondition::Block(activation) => block_number >= *activation,
                ForkCondition::Timestamp(activation) => timestamp >= *activation,
            })
            .map(|activation| activation.hardfork.clone())
    }
}

impl<HardforkT: Clone> From<&[HardforkActivation<HardforkT>]> for HardforkActivations<HardforkT> {
    fn from(hardforks: &[HardforkActivation<HardforkT>]) -> Self {
        Self {
            hardforks: hardforks.to_vec(),
        }
    }
}

/// Configuration for an ERC-20 contract whose balances mirror native token balances.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeTokenMirror {
    /// The ERC-20 contract address whose balances mirror native token balances.
    pub token: Address,
    /// Optional decimal precision for the ERC-20 facade. Defaults to the native
    /// token precision of 18 decimals.
    #[serde(default)]
    pub decimals: Option<u8>,
    /// The storage slot of the ERC-20 balances mapping.
    pub balance_slot: U256,
}

impl NativeTokenMirror {
    const NATIVE_DECIMALS: u8 = 18;

    /// Returns the ERC-20 decimal precision.
    pub fn decimals(&self) -> u8 {
        self.decimals.unwrap_or(Self::NATIVE_DECIMALS)
    }

    /// Returns the storage key for the provided account's ERC-20 balance.
    pub fn balance_storage_key(&self, account: Address) -> U256 {
        let mut preimage = [0u8; 64];
        preimage[12..32].copy_from_slice(account.as_slice());
        preimage[32..64].copy_from_slice(&self.balance_slot.to_be_bytes::<32>());
        U256::from_be_slice(keccak256(preimage).as_slice())
    }

    /// Returns the storage key for the ERC-20 allowance mapping.
    pub fn allowance_storage_key(&self, owner: Address, spender: Address) -> U256 {
        let allowance_slot = self.balance_slot.saturating_add(U256::from(1));

        let mut owner_preimage = [0u8; 64];
        owner_preimage[12..32].copy_from_slice(owner.as_slice());
        owner_preimage[32..64].copy_from_slice(&allowance_slot.to_be_bytes::<32>());
        let owner_slot = keccak256(owner_preimage);

        let mut spender_preimage = [0u8; 64];
        spender_preimage[12..32].copy_from_slice(spender.as_slice());
        spender_preimage[32..64].copy_from_slice(owner_slot.as_slice());
        U256::from_be_slice(keccak256(spender_preimage).as_slice())
    }

    /// Converts a native token balance into the mirrored ERC-20 balance.
    pub fn native_to_erc20_balance(&self, balance: U256) -> U256 {
        match self.decimals().cmp(&Self::NATIVE_DECIMALS) {
            core::cmp::Ordering::Equal => balance,
            core::cmp::Ordering::Greater => balance.saturating_mul(pow10(
                self.decimals()
                    .checked_sub(Self::NATIVE_DECIMALS)
                    .expect("checked by cmp"),
            )),
            core::cmp::Ordering::Less => {
                balance
                    / pow10(
                        Self::NATIVE_DECIMALS
                            .checked_sub(self.decimals())
                            .expect("checked by cmp"),
                    )
            }
        }
    }

    /// Converts a mirrored ERC-20 balance into the native token balance.
    pub fn erc20_to_native_balance(&self, balance: U256) -> U256 {
        match self.decimals().cmp(&Self::NATIVE_DECIMALS) {
            core::cmp::Ordering::Equal => balance,
            core::cmp::Ordering::Greater => {
                balance
                    / pow10(
                        self.decimals()
                            .checked_sub(Self::NATIVE_DECIMALS)
                            .expect("checked by cmp"),
                    )
            }
            core::cmp::Ordering::Less => balance.saturating_mul(pow10(
                Self::NATIVE_DECIMALS
                    .checked_sub(self.decimals())
                    .expect("checked by cmp"),
            )),
        }
    }
}

fn pow10(exponent: u8) -> U256 {
    U256::from(10).pow(U256::from(exponent))
}

/// Type that stores the configuration for a chain.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainConfig<HardforkT> {
    /// Chain name
    pub name: String,
    /// Hardfork activations for the chain
    pub hardfork_activations: HardforkActivations<HardforkT>,
    /// Base fee param activations for the chain
    pub base_fee_params: BaseFeeParams<HardforkT>,
    /// Blob Parameter Only hardforks schedule
    pub bpo_hardfork_schedule: Option<ScheduledBlobParams>,
    /// Optional native token mirror configuration.
    #[serde(default)]
    pub native_token_mirror: Option<NativeTokenMirror>,
}

impl<HardforkT: Clone> ChainConfig<HardforkT> {
    /// Applies the provided override to the current instance, while keeping the
    /// name the same.
    pub fn apply_override(&mut self, override_config: &ChainOverride<HardforkT>) {
        if let Some(hardfork_activations) = &override_config.hardfork_activation_overrides {
            self.hardfork_activations = hardfork_activations.clone();
        }

        if let Some(native_token_mirror) = &override_config.native_token_mirror {
            self.native_token_mirror = Some(native_token_mirror.clone());
        }
    }
}

/// Type that stores the configuration for a chain.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainOverride<HardforkT> {
    /// Chain name
    pub name: String,
    /// Hardfork activations for the chain
    pub hardfork_activation_overrides: Option<HardforkActivations<HardforkT>>,
    /// Optional native token mirror configuration.
    #[serde(default)]
    pub native_token_mirror: Option<NativeTokenMirror>,
}
