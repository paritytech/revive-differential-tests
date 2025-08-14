use semver::Version;

/// This is the first version of solc that supports the `--via-ir` flag / "viaIR" input JSON.
pub const VERSION_SUPPORTING_VIA_IR: Version = Version::new(0, 8, 13);
