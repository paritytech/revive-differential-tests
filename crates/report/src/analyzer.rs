//! The report analyzer enriches the raw report data.

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
    /// Is a proxy because the YUL also containes a lot of comments.
    pub yul_to_bytecode_size_ratio: f32,
}

impl CompilerStatistics {
    pub fn update(&mut self, compilation_task: &CompilationTask) {
        let Some(output) = &compilation_task.json_output else {
            return;
        };

        let Some(contracts) = &output.contracts else {
            return;
        };

        for (_solidity, contracts) in contracts.iter() {
            for (_name, contract) in contracts.iter() {
                let Some(evm) = &contract.evm else {
                    continue;
                };
                let Some(deploy_code) = &evm.deployed_bytecode else {
                    continue;
                };
                let bytecode_size = hex::decode(&deploy_code.object)
                    .expect("the bytecode in the compiler output should be valid")
                    .len();

                let ir_optimized = contract.ir_optimized.as_ref().expect(
                    "if the contract has a deploy code it should also have the opimized IR",
                );

                self.update_sizes(bytecode_size, ir_optimized.len());
            }
        }
    }

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

        let mean_code_size = 41; // rounding error
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
