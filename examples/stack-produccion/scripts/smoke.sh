#!/usr/bin/env bash
set -euo pipefail
curl -fsS "http://${1:-localhost}:3000/health" >/dev/null
