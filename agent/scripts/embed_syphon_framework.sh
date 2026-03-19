#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SUBMODULE_DIR="${ROOT_DIR}/native/syphon/Syphon-Framework"
DERIVED_DATA="${ROOT_DIR}/target/syphon-xcodebuild"
PRODUCTS_DIR="${DERIVED_DATA}/Build/Products/Release"
FRAMEWORK_SRC="${PRODUCTS_DIR}/Syphon.framework"
TARGET_DEBUG_FRAMEWORKS="${ROOT_DIR}/target/debug/Frameworks"
TARGET_RELEASE_FRAMEWORKS="${ROOT_DIR}/target/release/Frameworks"

if [[ ! -d "${SUBMODULE_DIR}" ]]; then
  echo "Syphon submodule not found: ${SUBMODULE_DIR}" >&2
  exit 1
fi

echo "Building Syphon.framework from submodule..."
xcodebuild \
  -project "${SUBMODULE_DIR}/Syphon.xcodeproj" \
  -scheme "Syphon" \
  -configuration "Release" \
  -derivedDataPath "${DERIVED_DATA}" \
  build > /dev/null

if [[ ! -d "${FRAMEWORK_SRC}" ]]; then
  echo "Built framework not found: ${FRAMEWORK_SRC}" >&2
  exit 1
fi

mkdir -p "${TARGET_DEBUG_FRAMEWORKS}" "${TARGET_RELEASE_FRAMEWORKS}"
rm -rf "${TARGET_DEBUG_FRAMEWORKS}/Syphon.framework" "${TARGET_RELEASE_FRAMEWORKS}/Syphon.framework"
cp -R "${FRAMEWORK_SRC}" "${TARGET_DEBUG_FRAMEWORKS}/Syphon.framework"
cp -R "${FRAMEWORK_SRC}" "${TARGET_RELEASE_FRAMEWORKS}/Syphon.framework"

echo "Embedded Syphon.framework:"
echo "  ${TARGET_DEBUG_FRAMEWORKS}/Syphon.framework"
echo "  ${TARGET_RELEASE_FRAMEWORKS}/Syphon.framework"
