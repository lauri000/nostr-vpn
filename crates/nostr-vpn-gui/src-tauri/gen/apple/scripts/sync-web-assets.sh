#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
WEB_DIST_DIR="$(cd "${PROJECT_DIR}/../../.." && pwd)/dist"
ASSETS_DIR="${PROJECT_DIR}/assets"

if [[ ! -f "${WEB_DIST_DIR}/index.html" ]]; then
  echo "expected frontend build output at ${WEB_DIST_DIR}/index.html" >&2
  exit 1
fi

mkdir -p "${ASSETS_DIR}"
rsync -a --delete "${WEB_DIST_DIR}/" "${ASSETS_DIR}/"
