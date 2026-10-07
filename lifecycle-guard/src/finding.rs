use serde::Serialize;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Class {
    DirectWrite,
    WrapperWrite,
    DynamicVerb,
    LifecycleRead,
    Credential,
    RetiredLabel,
    DeletedPath,
    BriefBd,
    LandstateCall,
    LandstatePath,
    BdStatusRead,
    BdCloseRust,
    HoldLabel,
    BdReopenRust,
}

impl Class {
    pub fn as_str(self) -> &'static str {
        match self {
            Class::DirectWrite => "direct-write",
            Class::WrapperWrite => "wrapper-write",
            Class::DynamicVerb => "dynamic-verb",
            Class::LifecycleRead => "lifecycle-read",
            Class::Credential => "credential",
            Class::RetiredLabel => "retired-label",
            Class::DeletedPath => "deleted-path",
            Class::BriefBd => "brief-bd",
            Class::LandstateCall => "landstate-call",
            Class::LandstatePath => "landstate-path",
            Class::BdStatusRead => "bd-status-read",
            Class::BdCloseRust => "bd-close-rust",
            Class::HoldLabel => "hold-label",
            Class::BdReopenRust => "bd-reopen-rust",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub class: Class,
    pub file: String,
    pub line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callee: Option<String>,
    pub detail: String,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}: [{}] {}{}{}",
            self.file,
            self.line,
            self.class.as_str(),
            self.function
                .as_ref()
                .map(|fun| format!("in {fun}: "))
                .unwrap_or_default(),
            self.detail,
            self.callee
                .as_ref()
                .map(|c| format!(" (callee: {c})"))
                .unwrap_or_default(),
        )
    }
}
