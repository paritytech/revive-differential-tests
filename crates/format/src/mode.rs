use regex::Regex;
use revive_dt_common::types::{Mode, ModeOptimizerSetting, ModePipeline};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt::Display;
use std::str::FromStr;
use std::sync::LazyLock;

/// This represents a mode that has been parsed from test metadata.
///
/// Mode strings can take the following form (in pseudo-regex):
///
/// ```text
/// [YEILV][+-]? (M[0123sz])? <semver>?
/// ```
///
/// We can parse valid mode strings into [`ParsedMode`] using [`ParsedMode::from_str`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct ParsedMode {
    pub pipeline: Option<ModePipeline>,
    pub optimize_flag: Option<bool>,
    pub optimize_setting: Option<ModeOptimizerSetting>,
    pub version: Option<semver::VersionReq>,
}

impl FromStr for ParsedMode {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        static REGEX: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(?x)
                ^
                (?:(?P<pipeline>[YEILV])(?P<optimize_flag>[+-])?)? # Pipeline to use eg Y, E+, E-
                \s*
                (?P<optimize_setting>M[a-zA-Z0-9])?                # Optimize setting eg M0, Ms, Mz
                \s*
                (?P<version>[>=<]*\d+(?:\.\d+)*)?                  # Optional semver version eg >=0.8.0, 0.7, <0.8
                $
            ").unwrap()
        });

        let Some(caps) = REGEX.captures(s) else {
            anyhow::bail!("Cannot parse mode '{s}' from string");
        };

        let pipeline = match caps.name("pipeline") {
            Some(m) => Some(ModePipeline::from_str(m.as_str())?),
            None => None,
        };

        let optimize_flag = caps.name("optimize_flag").map(|m| m.as_str() == "+");

        let optimize_setting = match caps.name("optimize_setting") {
            Some(m) => Some(ModeOptimizerSetting::from_str(m.as_str())?),
            None => None,
        };

        let version = match caps.name("version") {
            Some(m) => Some(semver::VersionReq::parse(m.as_str()).map_err(|e| {
                anyhow::anyhow!("Cannot parse the version requirement '{}': {e}", m.as_str())
            })?),
            None => None,
        };

        Ok(ParsedMode {
            pipeline,
            optimize_flag,
            optimize_setting,
            version,
        })
    }
}

impl Display for ParsedMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut has_written = false;

        if let Some(pipeline) = self.pipeline {
            pipeline.fmt(f)?;
            if let Some(optimize_flag) = self.optimize_flag {
                f.write_str(if optimize_flag { "+" } else { "-" })?;
            }
            has_written = true;
        }

        if let Some(optimize_setting) = self.optimize_setting {
            if has_written {
                f.write_str(" ")?;
            }
            optimize_setting.fmt(f)?;
            has_written = true;
        }

        if let Some(version) = &self.version {
            if has_written {
                f.write_str(" ")?;
            }
            version.fmt(f)?;
        }

        Ok(())
    }
}

impl From<ParsedMode> for String {
    fn from(parsed_mode: ParsedMode) -> Self {
        parsed_mode.to_string()
    }
}

impl TryFrom<String> for ParsedMode {
    type Error = anyhow::Error;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        ParsedMode::from_str(&value)
    }
}

impl ParsedMode {
    /// This takes a [`ParsedMode`] and expands it into a list of [`Mode`]s that we should try.
    pub fn to_modes(&self) -> impl Iterator<Item = Mode> {
        let pipeline_iter = self.pipeline.as_ref().map_or_else(
            || EitherIter::A(ModePipeline::test_cases()),
            |p| EitherIter::B(std::iter::once(*p)),
        );

        let optimize_flag_setting = self.optimize_flag.map(|flag| {
            if flag {
                ModeOptimizerSetting::M3
            } else {
                ModeOptimizerSetting::M0
            }
        });

        let optimize_flag_iter = match optimize_flag_setting {
            Some(setting) => EitherIter::A(std::iter::once(setting)),
            None => EitherIter::B(ModeOptimizerSetting::test_cases()),
        };

        let optimize_settings_iter = self.optimize_setting.as_ref().map_or_else(
            || EitherIter::A(optimize_flag_iter),
            |s| EitherIter::B(std::iter::once(*s)),
        );

        pipeline_iter.flat_map(move |pipeline| {
            optimize_settings_iter
                .clone()
                .map(move |optimize_setting| Mode {
                    pipeline,
                    optimize_setting,
                    version: self.version.clone(),
                })
        })
    }

    /// Return a set of [`Mode`]s that correspond to the given [`ParsedMode`]s.
    /// This avoids any duplicate entries.
    pub fn many_to_modes<'a>(
        parsed: impl Iterator<Item = &'a ParsedMode>,
    ) -> impl Iterator<Item = Mode> {
        let modes: HashSet<_> = parsed.flat_map(|p| p.to_modes()).collect();
        modes.into_iter()
    }
}

/// An iterator that could be either of two iterators.
#[derive(Clone, Debug)]
enum EitherIter<A, B> {
    A(A),
    B(B),
}

impl<A, B> Iterator for EitherIter<A, B>
where
    A: Iterator,
    B: Iterator<Item = A::Item>,
{
    type Item = A::Item;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            EitherIter::A(iter) => iter.next(),
            EitherIter::B(iter) => iter.next(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parsed_mode_from_str() {
        let strings = vec![
            ("Mz", "Mz"),
            ("Y", "Y"),
            ("Y+", "Y+"),
            ("Y-", "Y-"),
            ("E", "E"),
            ("E+", "E+"),
            ("E-", "E-"),
            ("Y M0", "Y M0"),
            ("Y M1", "Y M1"),
            ("Y M2", "Y M2"),
            ("Y M3", "Y M3"),
            ("Y Ms", "Y Ms"),
            ("Y Mz", "Y Mz"),
            ("E M0", "E M0"),
            ("E M1", "E M1"),
            ("E M2", "E M2"),
            ("E M3", "E M3"),
            ("E Ms", "E Ms"),
            ("E Mz", "E Mz"),
            // When stringifying semver again, 0.8.0 becomes ^0.8.0 (same meaning)
            ("Y 0.8.0", "Y ^0.8.0"),
            ("E+ 0.8.0", "E+ ^0.8.0"),
            ("Y M3 >=0.8.0", "Y M3 >=0.8.0"),
            ("E Mz <0.7.0", "E Mz <0.7.0"),
            // We can parse +- _and_ M1/M2 but the latter takes priority.
            ("Y+ M1 0.8.0", "Y+ M1 ^0.8.0"),
            ("E- M2 0.7.0", "E- M2 ^0.7.0"),
            // We don't see this in the wild but it is parsed.
            ("<=0.8", "<=0.8"),
        ];

        for (actual, expected) in strings {
            let parsed = ParsedMode::from_str(actual)
                .unwrap_or_else(|_| panic!("Failed to parse mode string '{actual}'"));
            assert_eq!(
                expected,
                parsed.to_string(),
                "Mode string '{actual}' did not parse to '{expected}': got '{parsed}'"
            );
        }
    }

    #[test]
    fn test_parsed_mode_to_test_modes() {
        let strings = vec![
            ("Mz", vec!["Y Mz", "E Mz"]),
            ("Y", vec!["Y M0", "Y M3"]),
            ("E", vec!["E M0", "E M3"]),
            ("Y+", vec!["Y M3"]),
            ("Y-", vec!["Y M0"]),
            ("Y <=0.8", vec!["Y M0 <=0.8", "Y M3 <=0.8"]),
            (
                "<=0.8",
                vec!["Y M0 <=0.8", "Y M3 <=0.8", "E M0 <=0.8", "E M3 <=0.8"],
            ),
        ];

        for (actual, expected) in strings {
            let parsed = ParsedMode::from_str(actual)
                .unwrap_or_else(|_| panic!("Failed to parse mode string '{actual}'"));
            let expected_set: HashSet<_> = expected.into_iter().map(|s| s.to_owned()).collect();
            let actual_set: HashSet<_> = parsed.to_modes().map(|m| m.to_string()).collect();

            assert_eq!(
                expected_set, actual_set,
                "Mode string '{actual}' did not expand to '{expected_set:?}': got '{actual_set:?}'"
            );
        }
    }
}
