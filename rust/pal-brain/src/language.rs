//! The conversation language: what whisper listens for, how Pal is prompted and which voice speaks.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Language {
    #[default]
    German,
    English,
}

impl Language {
    /// ISO 639-1 code, as whisper and the system voices use it.
    pub fn code(self) -> &'static str {
        match self {
            Language::German => "de",
            Language::English => "en",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "de" => Some(Language::German),
            "en" => Some(Language::English),
            _ => None,
        }
    }
}
