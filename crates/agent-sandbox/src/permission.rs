//! What a tool is allowed to affect, which is the only question the approval
//! system and the sandbox both need answered.
//!
//! The class is a property of the tool, not of the run: `fs_read_file` is
//! [`Permission::ReadOnly`] whoever calls it. Everything else — whether this
//! particular action needs asking, whether the path is inside the workspace —
//! is decided against the class, so the rules stay in one place.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Permission {
    /// Reads something. Never asks, in any approval mode but `suggest-only`
    /// having nothing to propose.
    ReadOnly,
    /// Creates or changes a file.
    Write,
    /// Runs a program.
    Execute,
    /// Reaches the network.
    Network,
    /// Removes something that cannot be recovered by undoing a write.
    Destructive,
}

impl Permission {
    pub const ALL: [Self; 5] = [
        Self::ReadOnly,
        Self::Write,
        Self::Execute,
        Self::Network,
        Self::Destructive,
    ];

    /// The floor of caution: everything but a read can go wrong, and a
    /// `suggest-only` run asks about nothing because it does nothing.
    pub fn is_read_only(self) -> bool {
        self == Self::ReadOnly
    }

    /// Whether a policy has to be consulted at all. Reads are the only class
    /// that never needs one, which is what keeps the loop quiet.
    pub fn needs_policy(self) -> bool {
        !self.is_read_only()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Write => "write",
            Self::Execute => "execute",
            Self::Network => "network",
            Self::Destructive => "destructive",
        }
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Permission {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|class| class.as_str() == s)
            .ok_or_else(|| {
                format!(
                    "unknown permission class {s:?}, expected one of {}",
                    Self::ALL.map(|class| class.as_str()).join(", ")
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_reads_skip_the_policy() {
        assert!(!Permission::ReadOnly.needs_policy());
        for class in [
            Permission::Write,
            Permission::Execute,
            Permission::Network,
            Permission::Destructive,
        ] {
            assert!(class.needs_policy(), "{class} must be checked");
        }
    }

    #[test]
    fn a_name_that_is_not_a_class_is_refused() {
        assert_eq!(
            "read-only".parse::<Permission>().ok(),
            Some(Permission::ReadOnly)
        );
        assert!(
            "readonly".parse::<Permission>().is_err(),
            "a typo is not a class"
        );
        assert!("delete".parse::<Permission>().is_err());
    }

    #[test]
    fn the_wire_spelling_is_kebab_case() {
        let json = serde_json::to_string(&Permission::ReadOnly).expect("serialisable");
        assert_eq!(json, "\"read-only\"");
        assert_eq!(
            serde_json::from_str::<Permission>("\"destructive\"").expect("parses"),
            Permission::Destructive
        );
    }
}
