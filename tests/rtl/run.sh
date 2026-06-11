#!/usr/bin/env bash
# Bootstrap the venv on first run, then execute the RTL test suite.
# Usage: tests/rtl/run.sh [pytest args]
set -euo pipefail
cd "$(dirname "$0")"
if [ ! -x .venv/bin/pytest ]; then
    python3 -m venv .venv 2>/dev/null || virtualenv .venv
    .venv/bin/pip install -q -r requirements.txt
fi
exec .venv/bin/pytest "$@"
