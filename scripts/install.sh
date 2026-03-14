#!/usr/bin/env bash
set -Eeuo pipefail
IFS=$'\n\t'

# install.sh — install rpg from GitHub releases
# Usage: curl -fsSL https://raw.githubusercontent.com/NikolayS/project-alpha/main/scripts/install.sh | bash

GITHUB_REPO="NikolayS/project-alpha"
BINARY_NAME="rpg"
GITHUB_API="https://api.github.com/repos/${GITHUB_REPO}/releases/latest"

tmp_dir=""

cleanup() {
  if [[ -n "${tmp_dir}" && -d "${tmp_dir}" ]]; then
    rm -rf "${tmp_dir}"
  fi
}

trap cleanup EXIT

die() {
  echo "error: $*" >&2
  exit 1
}

info() {
  echo "--> $*"
}

# Detect download tool: prefer curl, fall back to wget
download() {
  local url="${1}"
  local dest="${2}"
  if command -v curl > /dev/null 2>&1; then
    curl --fail --silent --show-error --location \
      --output "${dest}" \
      "${url}"
  elif command -v wget > /dev/null 2>&1; then
    wget --quiet --output-document="${dest}" "${url}"
  else
    die "neither curl nor wget found; please install one"
  fi
}

detect_os() {
  local os
  os="$(uname -s)"
  case "${os}" in
    Linux)  echo "linux" ;;
    Darwin) echo "darwin" ;;
    *)      die "unsupported operating system: ${os}" ;;
  esac
}

detect_arch() {
  local arch
  arch="$(uname -m)"
  case "${arch}" in
    x86_64 | amd64)          echo "x86_64" ;;
    aarch64 | arm64)         echo "aarch64" ;;
    *)                       die "unsupported architecture: ${arch}" ;;
  esac
}

# Map (os, arch) to the GitHub release asset name
asset_name() {
  local os="${1}"
  local arch="${2}"
  case "${os}-${arch}" in
    linux-x86_64)   echo "${BINARY_NAME}-x86_64-unknown-linux-gnu" ;;
    linux-aarch64)  echo "${BINARY_NAME}-aarch64-unknown-linux-gnu" ;;
    darwin-x86_64)  echo "${BINARY_NAME}-x86_64-apple-darwin" ;;
    darwin-aarch64) echo "${BINARY_NAME}-aarch64-apple-darwin" ;;
    *)              die "no release asset for ${os}-${arch}" ;;
  esac
}

fetch_latest_version() {
  local tmp_json="${tmp_dir}/release.json"
  download "${GITHUB_API}" "${tmp_json}"
  # Parse tag_name without jq by matching the first occurrence
  local tag
  tag="$(grep -m1 '"tag_name"' "${tmp_json}" \
    | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')"
  if [[ -z "${tag}" ]]; then
    die "could not determine latest release tag from GitHub API"
  fi
  echo "${tag}"
}

verify_sha256() {
  local file="${1}"
  local expected="${2}"
  local actual
  if command -v sha256sum > /dev/null 2>&1; then
    actual="$(sha256sum "${file}" | awk '{print $1}')"
  elif command -v shasum > /dev/null 2>&1; then
    actual="$(shasum -a 256 "${file}" | awk '{print $1}')"
  else
    die "neither sha256sum nor shasum found; cannot verify checksum"
  fi
  if [[ "${actual}" != "${expected}" ]]; then
    die "checksum mismatch for ${file}
  expected: ${expected}
  got:      ${actual}"
  fi
}

install_dir() {
  if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
    echo "/usr/local/bin"
  else
    echo "${HOME}/.local/bin"
  fi
}

main() {
  info "detecting platform..."
  local os arch asset version
  os="$(detect_os)"
  arch="$(detect_arch)"
  asset="$(asset_name "${os}" "${arch}")"

  info "fetching latest release version..."
  tmp_dir="$(mktemp -d)"
  version="$(fetch_latest_version)"
  info "latest version: ${version}"

  local base_url
  base_url="https://github.com/${GITHUB_REPO}/releases/download/${version}"
  local binary_url="${base_url}/${asset}"
  local checksums_url="${base_url}/checksums.txt"

  info "downloading ${asset}..."
  local binary_path="${tmp_dir}/${asset}"
  download "${binary_url}" "${binary_path}"

  info "downloading checksums..."
  local checksums_path="${tmp_dir}/checksums.txt"
  download "${checksums_url}" "${checksums_path}"

  info "verifying checksum..."
  local expected_hash
  expected_hash="$(grep -F "${asset}" "${checksums_path}" | awk '{print $1}')"
  if [[ -z "${expected_hash}" ]]; then
    die "no checksum entry found for ${asset} in checksums.txt"
  fi
  verify_sha256 "${binary_path}" "${expected_hash}"
  info "checksum OK"

  local dest_dir
  dest_dir="$(install_dir)"
  mkdir -p "${dest_dir}"

  local dest="${dest_dir}/${BINARY_NAME}"
  info "installing to ${dest}..."
  cp "${binary_path}" "${dest}"
  chmod +x "${dest}"

  info "installed ${BINARY_NAME} ${version} -> ${dest}"

  # Warn if dest_dir is not on PATH
  case ":${PATH}:" in
    *":${dest_dir}:"*) ;;
    *) echo "warning: ${dest_dir} is not in PATH;" \
         "add it to your shell config" >&2 ;;
  esac
}

main "$@"
