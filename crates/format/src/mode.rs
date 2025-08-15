use std::{fmt::Display, str::FromStr};

use revive_dt_common::types::VersionOrRequirement;
use semver::Version;
use serde::Serialize;

/// Specifies a compilation mode for the test artifact that it requires. This is used as a filter
/// when used in the [`Metadata`] and is used as a directive when used in the core crate.
///
/// [`Metadata`]: crate::metadata::Metadata
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    /// A compilation mode that's been parsed from a String and into its contents.
    Solidity(SolcMode),
    /// An unknown compilation mode.
    Unknown(String),
}

impl From<Mode> for String {
    fn from(value: Mode) -> Self {
        value.to_string()
    }
}

impl Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mode::Solidity(mode) => mode.fmt(f),
            Mode::Unknown(string) => string.fmt(f),
        }
    }
}

impl Mode {
    pub fn parse_from_string(str: impl AsRef<str>) -> Vec<Self> {
        let mut chars = str.as_ref().chars().peekable();

        let compile_via_ir = match chars.next() {
            Some('Y') => true,
            Some('E') => false,
            _ => {
                tracing::warn!("Encountered an unknown mode {}", str.as_ref());
                return vec![Self::Unknown(str.as_ref().to_string())];
            }
        };

        let optimize_flag = match chars.peek() {
            Some('+') => {
                let _ = chars.next();
                Some(true)
            }
            Some('-') => {
                let _ = chars.next();
                Some(false)
            }
            _ => None,
        };

        let mut chars = chars.skip_while(|char| *char == ' ').peekable();

        let version_requirement = match chars.peek() {
            Some('=' | '>' | '<' | '~' | '^' | '*' | '0'..='9') => {
                let version_requirement = chars.take_while(|char| *char != ' ').collect::<String>();
                let Ok(version_requirement) = VersionOrRequirement::from_str(&version_requirement)
                else {
                    return vec![Self::Unknown(str.as_ref().to_string())];
                };
                Some(version_requirement)
            }
            _ => None,
        };

        match optimize_flag {
            Some(flag) => {
                vec![Self::Solidity(SolcMode {
                    via_ir: compile_via_ir,
                    optimize: flag,
                    compiler_version_requirement: version_requirement,
                })]
            }
            None => {
                vec![
                    Self::Solidity(SolcMode {
                        via_ir: compile_via_ir,
                        optimize: true,
                        compiler_version_requirement: version_requirement.clone(),
                    }),
                    Self::Solidity(SolcMode {
                        via_ir: compile_via_ir,
                        optimize: false,
                        compiler_version_requirement: version_requirement,
                    }),
                ]
            }
        }
    }

    pub fn matches(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Mode::Solidity(SolcMode {
                    via_ir: self_via_ir,
                    optimize: self_optimize,
                    compiler_version_requirement: self_compiler_version_requirement,
                }),
                Mode::Solidity(SolcMode {
                    via_ir: other_via_ir,
                    optimize: other_optimize,
                    compiler_version_requirement: other_compiler_version_requirement,
                }),
            ) => {
                let mut matches = true;
                matches &= self_via_ir == other_via_ir;
                matches &= self_optimize == other_optimize;
                match (
                    self_compiler_version_requirement,
                    other_compiler_version_requirement,
                ) {
                    (
                        Some(VersionOrRequirement::Version(self_version)),
                        Some(VersionOrRequirement::Version(other_version)),
                    ) => {
                        matches &= self_version == other_version;
                    }
                    (
                        Some(VersionOrRequirement::Version(version)),
                        Some(VersionOrRequirement::Requirement(requirement)),
                    )
                    | (
                        Some(VersionOrRequirement::Requirement(requirement)),
                        Some(VersionOrRequirement::Version(version)),
                    ) => matches &= requirement.matches(version),
                    (
                        Some(VersionOrRequirement::Requirement(..)),
                        Some(VersionOrRequirement::Requirement(..)),
                    ) => matches = false,
                    (Some(_), None) | (None, Some(_)) | (None, None) => {}
                }
                matches
            }
            (Mode::Solidity { .. }, Mode::Unknown(_))
            | (Mode::Unknown(_), Mode::Solidity { .. })
            | (Mode::Unknown(_), Mode::Unknown(_)) => false,
        }
    }

    pub fn as_solc_mode(&self) -> Option<&SolcMode> {
        if let Self::Solidity(mode) = self {
            Some(mode)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(into = "String")]
pub struct SolcMode {
    pub via_ir: bool,
    pub optimize: bool,
    pub compiler_version_requirement: Option<VersionOrRequirement>,
}

impl SolcMode {
    pub const ALL: [Self; 4] = [
        SolcMode {
            via_ir: false,
            optimize: false,
            compiler_version_requirement: None,
        },
        SolcMode {
            via_ir: false,
            optimize: true,
            compiler_version_requirement: None,
        },
        SolcMode {
            via_ir: true,
            optimize: false,
            compiler_version_requirement: None,
        },
        SolcMode {
            via_ir: true,
            optimize: true,
            compiler_version_requirement: None,
        },
    ];

    pub fn matches(&self, other: &Self) -> bool {
        Mode::Solidity(self.clone()).matches(&Mode::Solidity(other.clone()))
    }

    /// Resolves the [`SolcMode`]'s solidity version requirement into a [`VersionOrRequirement`] if
    /// the requirement is present on the object. Otherwise, the passed default version is used.
    pub fn compiler_version_to_use(&self, default: Version) -> VersionOrRequirement {
        match self.compiler_version_requirement {
            Some(ref requirement) => requirement.clone(),
            None => default.into(),
        }
    }
}

impl Display for SolcMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            via_ir,
            optimize,
            compiler_version_requirement,
        } = self;

        if *via_ir {
            write!(f, "Y")?;
        } else {
            write!(f, "E")?;
        }

        if *optimize {
            write!(f, "+")?;
        } else {
            write!(f, "-")?;
        }

        if let Some(req) = compiler_version_requirement {
            write!(f, " {req}")?;
        }

        Ok(())
    }
}

impl From<SolcMode> for String {
    fn from(value: SolcMode) -> Self {
        value.to_string()
    }
}

#[cfg(test)]
mod test {
    use semver::Version;

    use super::*;

    #[test]
    fn mode_can_be_parsed_as_expected() {
        // Arrange
        let fixtures = [
            (
                "Y",
                vec![
                    Mode::Solidity(SolcMode {
                        via_ir: true,
                        optimize: true,
                        compiler_version_requirement: None,
                    }),
                    Mode::Solidity(SolcMode {
                        via_ir: true,
                        optimize: false,
                        compiler_version_requirement: None,
                    }),
                ],
            ),
            (
                "Y+",
                vec![Mode::Solidity(SolcMode {
                    via_ir: true,
                    optimize: true,
                    compiler_version_requirement: None,
                })],
            ),
            (
                "Y-",
                vec![Mode::Solidity(SolcMode {
                    via_ir: true,
                    optimize: false,
                    compiler_version_requirement: None,
                })],
            ),
            (
                "E",
                vec![
                    Mode::Solidity(SolcMode {
                        via_ir: false,
                        optimize: true,
                        compiler_version_requirement: None,
                    }),
                    Mode::Solidity(SolcMode {
                        via_ir: false,
                        optimize: false,
                        compiler_version_requirement: None,
                    }),
                ],
            ),
            (
                "E+",
                vec![Mode::Solidity(SolcMode {
                    via_ir: false,
                    optimize: true,
                    compiler_version_requirement: None,
                })],
            ),
            (
                "E-",
                vec![Mode::Solidity(SolcMode {
                    via_ir: false,
                    optimize: false,
                    compiler_version_requirement: None,
                })],
            ),
            (
                "Y >=0.8.3",
                vec![
                    Mode::Solidity(SolcMode {
                        via_ir: true,
                        optimize: true,
                        compiler_version_requirement: Some(VersionOrRequirement::Requirement(
                            ">=0.8.3".parse().unwrap(),
                        )),
                    }),
                    Mode::Solidity(SolcMode {
                        via_ir: true,
                        optimize: false,
                        compiler_version_requirement: Some(VersionOrRequirement::Requirement(
                            ">=0.8.3".parse().unwrap(),
                        )),
                    }),
                ],
            ),
            (
                "Y 0.8.3",
                vec![
                    Mode::Solidity(SolcMode {
                        via_ir: true,
                        optimize: true,
                        compiler_version_requirement: Some(VersionOrRequirement::Version(
                            Version {
                                major: 0,
                                minor: 8,
                                patch: 3,
                                pre: Default::default(),
                                build: Default::default(),
                            },
                        )),
                    }),
                    Mode::Solidity(SolcMode {
                        via_ir: true,
                        optimize: false,
                        compiler_version_requirement: Some(VersionOrRequirement::Version(
                            Version {
                                major: 0,
                                minor: 8,
                                patch: 3,
                                pre: Default::default(),
                                build: Default::default(),
                            },
                        )),
                    }),
                ],
            ),
        ];

        for (string, expectation) in fixtures {
            // Act
            let actual = Mode::parse_from_string(string);

            // Assert
            assert_eq!(
                actual, expectation,
                "Parsed {string} into {actual:?} but expected {expectation:?}"
            )
        }
    }

    #[test]
    #[allow(clippy::uninlined_format_args)]
    fn mode_matches_as_expected() {
        // Arrange
        let fixtures = [("Y+", "Y+", true), ("Y+ >=0.8.3", "Y+", true)];

        for (self_mode, other_mode, expected_result) in fixtures {
            let self_mode = Mode::parse_from_string(self_mode).pop().unwrap();
            let other_mode = Mode::parse_from_string(other_mode).pop().unwrap();

            // Act
            let actual = self_mode.matches(&other_mode);

            // Assert
            assert_eq!(
                actual, expected_result,
                "Match of {} and {} failed. Expected {} but got {}",
                self_mode, other_mode, expected_result, actual
            );
        }
    }
}
