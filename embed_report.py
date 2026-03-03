#!/usr/bin/env python3
"""Embed JSON report data into the benchmark report HTML template."""

import sys
from pathlib import Path

TEMPLATE = Path("crates/report/templates/benchmark-report.html")
REPORT_JSON = Path("workdir/report-multi.json")
OUTPUT = Path("workdir/benchmark-report.html")

PLACEHOLDER = "/*__REPORT_DATA__*/"


def main():
    template = TEMPLATE.read_text()
    if PLACEHOLDER not in template:
        print(f"Error: placeholder {PLACEHOLDER} not found in template", file=sys.stderr)
        sys.exit(1)

    report_data = REPORT_JSON.read_text()
    html = template.replace(PLACEHOLDER, report_data, 1)

    OUTPUT.write_text(html)
    print(f"Wrote {OUTPUT} ({len(html):,} bytes)")


if __name__ == "__main__":
    main()
