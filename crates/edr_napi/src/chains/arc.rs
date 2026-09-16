use std::sync::Arc;

use edr_generic::{ArcChainSpec, ArcHardfork};
use edr_napi_core::{logger::Logger, provider::SyncProvider};
use edr_provider::time::CurrentTime;
use edr_solidity::contract_decoder::ContractDecoder;
use napi::tokio::runtime;
use napi_derive::napi;
use parking_lot::RwLock;

use crate::{
    provider::{factory::SyncProviderFactory, ProviderFactory},
    subscription::{subscriber_callback_for_chain_spec, SubscriptionTsfn},
};

pub struct ArcChainProviderFactory;

impl SyncProviderFactory for ArcChainProviderFactory {
    fn create_provider(
        &self,
        runtime: runtime::Handle,
        provider_config: edr_napi_core::provider::Config,
        logger_config: edr_napi_core::logger::Config,
        subscription_callback: Arc<SubscriptionTsfn>,
        contract_decoder: Arc<RwLock<ContractDecoder>>,
    ) -> napi::Result<Arc<dyn SyncProvider>> {
        let logger =
            Logger::<ArcChainSpec, CurrentTime>::new(logger_config, Arc::clone(&contract_decoder))?;

        let provider_config =
            edr_provider::config::Provider::<ArcHardfork>::try_from(provider_config)?;

        let provider = edr_provider::Provider::<ArcChainSpec>::new(
            runtime.clone(),
            Box::new(logger),
            subscriber_callback_for_chain_spec::<ArcChainSpec, CurrentTime>(subscription_callback),
            provider_config,
            contract_decoder,
            CurrentTime,
        )
        .map_err(|error| napi::Error::new(napi::Status::GenericFailure, error.to_string()))?;

        Ok(Arc::new(provider))
    }
}

#[napi]
pub const ARC_CHAIN_TYPE: &str = edr_generic::ARC_CHAIN_TYPE;

#[napi(catch_unwind)]
pub fn arc_chain_provider_factory() -> ProviderFactory {
    let factory: Arc<dyn SyncProviderFactory> = Arc::new(ArcChainProviderFactory);
    factory.into()
}
