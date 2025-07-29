//! The report analyzer enriches the raw report data.

use revive_dt_compiler::CompilerOutput;
use serde::{Deserialize, Serialize};

use crate::reporter::CompilationTask;

/// Provides insights into how well the compilers perform.
#[derive(Clone, Default, Debug, Serialize, Deserialize, PartialEq, PartialOrd)]
pub struct CompilerStatistics {
    /// The sum of contracts observed.
    pub n_contracts: usize,
    /// The mean size of compiled contracts.
    pub mean_code_size: usize,
    /// The mean size of the optimized YUL IR.
    pub mean_yul_size: usize,
    /// Is a proxy because the YUL also contains a lot of comments.
    pub yul_to_bytecode_size_ratio: f32,
}

impl CompilerStatistics {
    /// Cumulatively update the statistics with the next compiler task.
    pub fn sample(&mut self, compilation_task: &CompilationTask) {
        let Some(CompilerOutput { contracts }) = &compilation_task.json_output else {
            return;
        };

        for (_solidity, contracts) in contracts.iter() {
            for (_name, (bytecode, _)) in contracts.iter() {
                // The EVM bytecode can be unlinked and thus is not necessarily a decodable hex
                // string; for our statistics this is a good enough approximation.
                let bytecode_size = bytecode.len() / 2;

                // TODO: for the time being we set the yul_size to be zero. We need to change this
                // when we overhaul the reporting.

                self.update_sizes(bytecode_size, 0);
            }
        }
    }

    /// Updates the size statistics cumulatively.
    fn update_sizes(&mut self, bytecode_size: usize, yul_size: usize) {
        let n_previous = self.n_contracts;
        let n_current = self.n_contracts + 1;

        self.n_contracts = n_current;

        self.mean_code_size = (n_previous * self.mean_code_size + bytecode_size) / n_current;
        self.mean_yul_size = (n_previous * self.mean_yul_size + yul_size) / n_current;

        if self.mean_code_size > 0 {
            self.yul_to_bytecode_size_ratio =
                self.mean_yul_size as f32 / self.mean_code_size as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CompilerStatistics;

    #[test]
    fn compiler_statistics() {
        let mut received = CompilerStatistics::default();
        received.update_sizes(0, 0);
        received.update_sizes(3, 37);
        received.update_sizes(123, 456);

        let mean_code_size = 41; // rounding error from integer truncation
        let mean_yul_size = 164;
        let expected = CompilerStatistics {
            n_contracts: 3,
            mean_code_size,
            mean_yul_size,
            yul_to_bytecode_size_ratio: mean_yul_size as f32 / mean_code_size as f32,
        };

        assert_eq!(received, expected);
    }
}
