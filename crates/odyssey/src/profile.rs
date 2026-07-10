// GENERATED FROM odyssey — DO NOT EDIT
use std::fmt;

/// Cross-product presentation profiles supported by the Odyssey 1.2 root contract.
///
/// A profile coordinates shell framing, navigation, and status language. It does not replace a
/// product's ordinary semantic theme tokens. Bespoke templates opt in by stamping
/// `data-ody-profile="…"` on a root element; the standard shell helpers expose the same contract
/// through [`Profile::as_str`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Ai,
    Communication,
    Content,
    Control,
    Data,
    Developer,
    Identity,
    Knowledge,
    Networking,
    Observability,
    Portal,
    Productivity,
    Public,
    Security,
}

impl Profile {
    pub const ALL: [Self; 14] = [
        Self::Ai,
        Self::Communication,
        Self::Content,
        Self::Control,
        Self::Data,
        Self::Developer,
        Self::Identity,
        Self::Knowledge,
        Self::Networking,
        Self::Observability,
        Self::Portal,
        Self::Productivity,
        Self::Public,
        Self::Security,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ai => "ai",
            Self::Communication => "communication",
            Self::Content => "content",
            Self::Control => "control",
            Self::Data => "data",
            Self::Developer => "developer",
            Self::Identity => "identity",
            Self::Knowledge => "knowledge",
            Self::Networking => "networking",
            Self::Observability => "observability",
            Self::Portal => "portal",
            Self::Productivity => "productivity",
            Self::Public => "public",
            Self::Security => "security",
        }
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn profile_contract_is_unique_sorted_and_css_backed() {
        let names: Vec<_> = Profile::ALL.into_iter().map(Profile::as_str).collect();
        let unique: BTreeSet<_> = names.iter().copied().collect();

        assert_eq!(names.len(), unique.len());
        assert!(names.windows(2).all(|pair| pair[0] < pair[1]));

        let css = crate::PROFILE_CSS;
        for name in names {
            assert!(
                css.contains(&format!("data-ody-profile=\"{name}\"")),
                "profile {name} must be represented by the 1.2 CSS contract"
            );
        }
    }
}
