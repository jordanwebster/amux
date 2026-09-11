//! Codex protocol values used by the provider runtime.

pub(crate) use model::{
    CodexSdkInput as CodexSdkV1Input, CodexSdkV1Args, CodexSdkV1Output,
    CodexSdkV1ReplayQuery,
};

pub const CODEX_SDK_V1: &str = model::CODEX_SDK_V1;
