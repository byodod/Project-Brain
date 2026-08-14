use std::{collections::BTreeMap, path::Path};

use brain_core::{AdapterCapabilities, AdapterKind, BrainConfig};
use brain_store::BrainStore;

use crate::{
    app::HookEvent,
    codex::CodexHookInput,
    pi::{self, ExtensionHookOutput},
    provider::ProviderTrustStatus,
};

const DSH_ADAPTER_VERSION: u16 = 1;

pub type DshHookInput = CodexHookInput;
pub type DshHookOutput = ExtensionHookOutput;

pub const fn capabilities() -> AdapterCapabilities {
    AdapterCapabilities::dsh()
}

pub fn handle_with_provider_trust(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    provider_trust: &BTreeMap<String, ProviderTrustStatus>,
    event: HookEvent,
    input: &DshHookInput,
) -> DshHookOutput {
    pi::handle_adapter_with_provider_trust(
        root,
        config,
        store,
        provider_trust,
        event,
        input,
        AdapterKind::Dsh,
        DSH_ADAPTER_VERSION,
        "dsh",
        true,
    )
}

pub fn failure_output(event: HookEvent, input: &DshHookInput, error: &str) -> DshHookOutput {
    pi::adapter_failure_output(event, error, true, input.stop_hook_active())
}

#[cfg(test)]
mod tests {
    use brain_core::{AdapterCapabilities, CapabilitySupport};

    #[test]
    fn dsh_claims_verified_tool_gate_and_stop_continuation() {
        let capabilities = AdapterCapabilities::dsh();
        assert_eq!(capabilities.deny_tool, CapabilitySupport::Supported);
        assert_eq!(
            capabilities.continue_after_stop,
            CapabilitySupport::Supported
        );
    }
}
