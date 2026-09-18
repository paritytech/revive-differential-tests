use crate::internal_prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineInformation {
    pub cpu_models: BTreeSet<String>,
    pub physical_cores: Option<NonZeroUsize>,
    pub logical_cpus: Option<NonZeroUsize>,
    pub available_parallelism: Option<NonZeroUsize>,
    pub reported_cpu_frequencies_mhz: BTreeSet<NonZeroU64>,
    pub memory_bytes: Option<NonZeroU64>,
    pub operating_system: Option<String>,
    pub kernel_version: Option<String>,
    pub architecture: String,
}

pub fn get() -> MachineInformation {
    let system = System::new_with_specifics(
        RefreshKind::nothing()
            .with_cpu(CpuRefreshKind::nothing())
            .with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    // Read Linux's MHz snapshot directly: sysinfo can fall back to BogoMIPS.
    #[cfg(target_os = "linux")]
    let reported_cpu_frequencies_mhz = fs::read_to_string("/proc/cpuinfo")
        .map(|contents| {
            contents
                .lines()
                .filter_map(|line| line.split_once(':'))
                .filter(|(name, _)| name.trim() == "cpu MHz")
                .filter_map(|(_, value)| value.trim().split('.').next()?.parse::<u64>().ok())
                .filter_map(NonZeroU64::new)
                .collect()
        })
        .unwrap_or_default();
    #[cfg(not(target_os = "linux"))]
    let reported_cpu_frequencies_mhz = BTreeSet::new();

    MachineInformation {
        cpu_models: system
            .cpus()
            .iter()
            .map(|cpu| cpu.brand().trim())
            .filter(|brand| !brand.is_empty())
            .map(str::to_owned)
            .collect(),
        physical_cores: System::physical_core_count().and_then(NonZeroUsize::new),
        logical_cpus: NonZeroUsize::new(system.cpus().len()),
        available_parallelism: available_parallelism().ok(),
        reported_cpu_frequencies_mhz,
        memory_bytes: NonZeroU64::new(system.total_memory()),
        operating_system: System::long_os_version(),
        kernel_version: System::kernel_version(),
        architecture: System::cpu_arch(),
    }
}
