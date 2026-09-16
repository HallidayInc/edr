use core::cmp::Ordering;

use edr_chain_spec::EvmSpecId;
use edr_primitives::UnknownHardfork;

use arc_execution_config::hardforks::ArcHardfork as ArcActivation;
pub use arc_execution_config::hardforks::ArcHardforkFlags as ArcFeatures;

#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ArcHardfork {
    Execution {
        evm_spec_id: EvmSpecId,
        features: ArcFeatures,
    },
    Activation(ArcActivation),
}

impl ArcHardfork {
    const THROUGH_ZERO3: ArcFeatures = ArcFeatures::ZERO3;
    const THROUGH_ZERO4: ArcFeatures = Self::THROUGH_ZERO3.union(ArcFeatures::ZERO4);
    const THROUGH_ZERO5: ArcFeatures = Self::THROUGH_ZERO4.union(ArcFeatures::ZERO5);
    const THROUGH_ZERO6: ArcFeatures = Self::THROUGH_ZERO5.union(ArcFeatures::ZERO6);
    const THROUGH_ZERO7: ArcFeatures = Self::THROUGH_ZERO6.union(ArcFeatures::ZERO7);

    pub const PRAGUE: Self = Self::new(EvmSpecId::PRAGUE, ArcFeatures::NONE);
    pub const OSAKA: Self = Self::new(EvmSpecId::OSAKA, ArcFeatures::NONE);
    pub const ZERO3: Self = Self::new(EvmSpecId::PRAGUE, Self::THROUGH_ZERO3);
    pub const ZERO4: Self = Self::new(EvmSpecId::PRAGUE, Self::THROUGH_ZERO4);
    pub const ZERO5_PRAGUE: Self = Self::new(EvmSpecId::PRAGUE, Self::THROUGH_ZERO5);
    pub const ZERO6_PRAGUE: Self = Self::new(EvmSpecId::PRAGUE, Self::THROUGH_ZERO6);
    pub const ZERO5: Self = Self::new(EvmSpecId::OSAKA, Self::THROUGH_ZERO5);
    pub const ZERO6: Self = Self::new(EvmSpecId::OSAKA, Self::THROUGH_ZERO6);
    pub const ZERO7: Self = Self::new(EvmSpecId::OSAKA, Self::THROUGH_ZERO7);
    pub const ZERO8: Self = Self::new(EvmSpecId::OSAKA, ArcFeatures::ALL);
    pub const LATEST: Self = Self::ZERO8;

    pub const fn new(evm_spec_id: EvmSpecId, features: ArcFeatures) -> Self {
        Self::Execution {
            evm_spec_id,
            features,
        }
    }

    pub(crate) const ACTIVATE_ZERO3: Self = Self::Activation(ArcActivation::Zero3);
    pub(crate) const ACTIVATE_ZERO4: Self = Self::Activation(ArcActivation::Zero4);
    pub(crate) const ACTIVATE_ZERO5: Self = Self::Activation(ArcActivation::Zero5);
    pub(crate) const ACTIVATE_ZERO6: Self = Self::Activation(ArcActivation::Zero6);
    pub(crate) const ACTIVATE_ZERO7: Self = Self::Activation(ArcActivation::Zero7);
    pub(crate) const ACTIVATE_ZERO8: Self = Self::Activation(ArcActivation::Zero8);

    const fn dimensions(self) -> (EvmSpecId, ArcFeatures) {
        match self {
            Self::Execution {
                evm_spec_id,
                features,
            } => (evm_spec_id, features),
            Self::Activation(activation) => (EvmSpecId::PRAGUE, ArcFeatures::only(activation)),
        }
    }

    pub const fn execution(self) -> Self {
        match self {
            Self::Execution { .. } => self,
            Self::Activation(ArcActivation::Zero3) => Self::ZERO3,
            Self::Activation(ArcActivation::Zero4) => Self::ZERO4,
            Self::Activation(ArcActivation::Zero5) => Self::ZERO5,
            Self::Activation(ArcActivation::Zero6) => Self::ZERO6,
            Self::Activation(ArcActivation::Zero7) => Self::ZERO7,
            Self::Activation(ArcActivation::Zero8) => Self::ZERO8,
            Self::Activation(_) => panic!("unsupported Arc hardfork"),
        }
    }

    pub(crate) const fn flags(self) -> ArcFeatures {
        self.dimensions().1
    }

    pub(crate) fn combine(active: impl IntoIterator<Item = Self>) -> Self {
        active.into_iter().fold(Self::PRAGUE, |combined, item| {
            let (combined_evm, combined_features) = combined.dimensions();
            let (item_evm, item_features) = item.dimensions();
            Self::new(
                combined_evm.max(item_evm),
                combined_features.union(item_features),
            )
        })
    }
}

impl Default for ArcHardfork {
    fn default() -> Self {
        Self::LATEST
    }
}

impl PartialEq for ArcHardfork {
    fn eq(&self, other: &Self) -> bool {
        self.dimensions() == other.dimensions()
    }
}

impl Eq for ArcHardfork {}

impl PartialOrd for ArcHardfork {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        let (self_evm, self_features) = self.dimensions();
        let (other_evm, other_features) = other.dimensions();
        let self_subset = other_features.contains(self_features);
        let other_subset = self_features.contains(other_features);
        let self_evm_before = self_evm <= other_evm;
        let other_evm_before = other_evm <= self_evm;

        match (
            self_subset && self_evm_before,
            other_subset && other_evm_before,
        ) {
            (true, true) => Some(Ordering::Equal),
            (true, false) => Some(Ordering::Less),
            (false, true) => Some(Ordering::Greater),
            (false, false) => None,
        }
    }
}

impl From<ArcHardfork> for EvmSpecId {
    fn from(value: ArcHardfork) -> Self {
        value.execution().dimensions().0
    }
}

impl core::str::FromStr for ArcHardfork {
    type Err = UnknownHardfork;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "prague" => Ok(Self::PRAGUE),
            "osaka" => Ok(Self::OSAKA),
            "zero3" | "zero-3" => Ok(Self::ACTIVATE_ZERO3),
            "zero4" | "zero-4" => Ok(Self::ACTIVATE_ZERO4),
            "zero5-prague" | "zero-5-prague" => Ok(Self::ZERO5_PRAGUE),
            "zero6-prague" | "zero-6-prague" => Ok(Self::ZERO6_PRAGUE),
            "zero5" | "zero-5" => Ok(Self::ACTIVATE_ZERO5),
            "zero6" | "zero-6" => Ok(Self::ACTIVATE_ZERO6),
            "zero7" | "zero-7" => Ok(Self::ACTIVATE_ZERO7),
            "zero8" | "zero-8" => Ok(Self::ACTIVATE_ZERO8),
            "latest" => Ok(Self::LATEST),
            _ => Err(UnknownHardfork),
        }
    }
}
