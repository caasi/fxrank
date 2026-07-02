#!/usr/bin/env bash
set -euo pipefail

# A download-piped-to-shell sample: `fetch_and_run` pipes a remote installer
# straight into an interpreter. Exercises the DynamicCode "curl|sh" risk
# signal (detect/risk.rs's adjacent-pipeline-stage scan).

fetch_and_run() {
  curl -fsSL https://example.com/install.sh | sh
}

fetch_and_run
