"""
Utilities to print benchmark metrics from a report JSON into CSV.

Usage:
  python scripts/print_benchmark_metrics_csv.py /absolute/path/to/report.json

The script prints, for each metadata path, case index, and mode combination,
CSV rows aligned to mined blocks with the following columns:
  - block_number
  - number_of_txs
  - tps (transaction_per_second)
  - gps (gas_per_second)
  - gas_block_fullness
  - ref_time (if available)
  - max_ref_time (if available)
  - proof_size (if available)
  - max_proof_size (if available)
  - ref_time_block_fullness (if available)
  - proof_size_block_fullness (if available)

Important nuance: TPS and GPS arrays have (number_of_blocks - 1) items. The
first block row has no TPS/GPS; the CSV leaves those cells empty for the first
row and aligns subsequent values to their corresponding next block.
"""

from __future__ import annotations

import json
import sys
import csv
from typing import List, Mapping, TypedDict


class EthereumMinedBlockInformation(TypedDict):
    """EVM block information extracted from the report.

    Attributes:
        block_number: The block height.
        block_timestamp: The UNIX timestamp of the block.
        mined_gas: Total gas used (mined) in the block.
        block_gas_limit: The gas limit of the block.
        transaction_hashes: List of transaction hashes included in the block.
    """

    block_number: int
    block_timestamp: int
    mined_gas: int
    block_gas_limit: int
    transaction_hashes: List[str]


class SubstrateMinedBlockInformation(TypedDict):
    """Substrate-specific block resource usage fields.

    Attributes:
        ref_time: The consumed ref time in the block.
        max_ref_time: The maximum ref time allowed for the block.
        proof_size: The consumed proof size in the block.
        max_proof_size: The maximum proof size allowed for the block.
    """

    ref_time: int
    max_ref_time: int
    proof_size: int
    max_proof_size: int


class MinedBlockInformation(TypedDict):
    """Block-level information for a mined block with both EVM and optional Substrate fields."""

    ethereum_block_information: EthereumMinedBlockInformation
    substrate_block_information: SubstrateMinedBlockInformation


class Metric(TypedDict):
    """Metric data of integer values keyed by platform identifier.

    Attributes:
        minimum: Single scalar minimum per platform.
        maximum: Single scalar maximum per platform.
        mean: Single scalar mean per platform.
        median: Single scalar median per platform.
        raw: Time-series (or list) of values per platform.
    """

    minimum: Mapping[str, int]
    maximum: Mapping[str, int]
    mean: Mapping[str, int]
    median: Mapping[str, int]
    raw: Mapping[str, List[int]]


class Metrics(TypedDict):
    """All metrics that may be present for a given execution report.

    Note that some metrics are optional and present only for specific platforms
    or execution modes.
    """

    transaction_per_second: Metric
    gas_per_second: Metric
    gas_block_fullness: Metric
    ref_time_block_fullness: Metric
    proof_size_block_fullness: Metric


class ExecutionReport(TypedDict):
    """Execution report for a mode containing mined blocks and metrics.

    Attributes:
        mined_block_information: Mapping from platform identifier to the list of
            mined blocks observed for that platform.
        metrics: The computed metrics for the execution.
    """

    mined_block_information: Mapping[str, List[MinedBlockInformation]]
    metrics: Metrics


class CaseReport(TypedDict):
    """Report for a single case, keyed by mode string."""

    mode_execution_reports: Mapping[str, ExecutionReport]


class MetadataFileReport(TypedDict):
    """Report subtree keyed by case indices for a metadata file path."""

    case_reports: Mapping[str, CaseReport]


class ReportRoot(TypedDict):
    """Top-level report schema with execution information keyed by metadata path."""

    execution_information: Mapping[str, MetadataFileReport]


BlockInformation = TypedDict(
    "BlockInformation",
    {
        "Block Number": int,
        "Timestamp": int,
        "Datetime": None,
        "Transaction Count": int,
        "TPS": int | None,
        "GPS": int | None,
        "Ref Time": int,
        "Max Ref Time": int,
        "Block Fullness Ref Time": int,
        "Proof Size": int,
        "Max Proof Size": int,
        "Block Fullness Proof Size": int,
    },
)
"""A typed dictionary used to hold all of the block information"""


def load_report(path: str) -> ReportRoot:
    """Load the report JSON from disk.

    Args:
        path: Absolute or relative filesystem path to the JSON report file.

    Returns:
        The parsed report as a typed dictionary structure.
    """

    with open(path, "r", encoding="utf-8") as f:
        data: ReportRoot = json.load(f)
    return data


def main() -> None:
    report_path: str = sys.argv[1]
    report: ReportRoot = load_report(report_path)

    # TODO: Remove this in the future, but for now, the target is fixed.
    target: str = "revive-dev-node-revm-solc"

    csv_writer = csv.writer(sys.stdout)

    for _, metadata_file_report in report["execution_information"].items():
        for _, case_report in metadata_file_report["case_reports"].items():
            for _, execution_report in case_report["mode_execution_reports"].items():
                blocks_information: list[MinedBlockInformation] = execution_report[
                    "mined_block_information"
                ][target]

                resolved_blocks: list[BlockInformation] = []
                for i, block_information in enumerate(blocks_information):
                    resolved_blocks.append(
                        {
                            "Block Number": block_information[
                                "ethereum_block_information"
                            ]["block_number"],
                            "Timestamp": block_information[
                                "ethereum_block_information"
                            ]["block_timestamp"],
                            "Datetime": None,
                            "Transaction Count": len(
                                block_information["ethereum_block_information"][
                                    "transaction_hashes"
                                ]
                            ),
                            "TPS": (
                                None
                                if i == 0
                                else execution_report["metrics"][
                                    "transaction_per_second"
                                ]["raw"][target][i - 1]
                            ),
                            "GPS": (
                                None
                                if i == 0
                                else execution_report["metrics"]["gas_per_second"][
                                    "raw"
                                ][target][i - 1]
                            ),
                            "Ref Time": block_information[
                                "substrate_block_information"
                            ]["ref_time"],
                            "Max Ref Time": block_information[
                                "substrate_block_information"
                            ]["max_ref_time"],
                            "Block Fullness Ref Time": execution_report["metrics"][
                                "ref_time_block_fullness"
                            ]["raw"][target][i],
                            "Proof Size": block_information[
                                "substrate_block_information"
                            ]["proof_size"],
                            "Max Proof Size": block_information[
                                "substrate_block_information"
                            ]["max_proof_size"],
                            "Block Fullness Proof Size": execution_report["metrics"][
                                "proof_size_block_fullness"
                            ]["raw"][target][i],
                        }
                    )

                csv_writer = csv.DictWriter(sys.stdout, resolved_blocks[0].keys())
                csv_writer.writeheader()
                csv_writer.writerows(resolved_blocks)


if __name__ == "__main__":
    main()
