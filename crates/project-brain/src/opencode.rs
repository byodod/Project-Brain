use std::{collections::BTreeMap, path::Path};

use brain_core::{AdapterCapabilities, AdapterKind, BrainConfig};
use brain_store::BrainStore;

use crate::{
    app::HookEvent,
    codex::CodexHookInput,
    pi::{self, ExtensionHookOutput},
    provider::ProviderTrustStatus,
};

const OPENCODE_ADAPTER_VERSION: u16 = 1;

pub type OpencodeHookInput = CodexHookInput;
pub type OpencodeHookOutput = ExtensionHookOutput;

pub const fn capabilities() -> AdapterCapabilities {
    AdapterCapabilities::opencode()
}

pub fn handle_with_provider_trust(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    provider_trust: &BTreeMap<String, ProviderTrustStatus>,
    event: HookEvent,
    input: &OpencodeHookInput,
) -> OpencodeHookOutput {
    pi::handle_adapter_with_provider_trust(
        root,
        config,
        store,
        provider_trust,
        event,
        input,
        AdapterKind::Opencode,
        OPENCODE_ADAPTER_VERSION,
        "opencode",
        false,
    )
}

pub fn failure_output(
    event: HookEvent,
    input: &OpencodeHookInput,
    error: &str,
) -> OpencodeHookOutput {
    pi::adapter_failure_output(event, error, false, input.stop_hook_active())
}

#[cfg(test)]
mod tests {
    use brain_core::{AdapterCapabilities, CapabilitySupport};

    #[test]
    fn opencode_does_not_claim_stop_continuation() {
        assert_eq!(
            AdapterCapabilities::opencode().continue_after_stop,
            CapabilitySupport::Unsupported
        );
    }
}
