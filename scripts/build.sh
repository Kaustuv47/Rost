#!/usr/bin/env bash
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$SCRIPT_DIR/.."

# ── Reproducible build (IEC 61508 §7.3.1 / ISO 26262 Part 8 §9.3) ────────────
#
# Two identical source trees built with the same toolchain must produce
# bit-identical binaries.  This requires:
#
#   SOURCE_DATE_EPOCH  — clamps all embedded timestamps to a fixed value.
#                        If not set by the caller, we derive it from the most
#                        recent git commit so the build is still meaningful.
#
#   RUSTFLAGS --remap-path-prefix  — strips the absolute workspace path from
#                        debug info so builds on different machines match.
#
# To verify: run scripts/build.sh twice and compare SHA-256 of the EFI binary.
#   sha256sum build/efi/boot/bootx64.efi   (run twice, hashes must match)

if [ -z "${SOURCE_DATE_EPOCH}" ]; then
    SOURCE_DATE_EPOCH="$(git -C "$ROOT" log -1 --pretty=%ct 2>/dev/null || echo 0)"
fi
export SOURCE_DATE_EPOCH

# Remap absolute source paths in debug info to reproducible relative paths.
# NOTE: RUSTFLAGS env var overrides .cargo/config.toml rustflags entirely, so
# each cargo invocation must include ALL required flags for that workspace.
REMAP="--remap-path-prefix=${ROOT}=/rost"

# Ring-3 server flags: no stdlib, no red-zone, static (non-PIE) relocation so
# that the ELF loader does not need to apply RELA relocations at load time.
SERVERS_FLAGS="${REMAP} -C link-arg=-nostdlib -C no-redzone -C relocation-model=static"

# Step 1a: Build hello-world first so the VFS can embed it via include_bytes!().
# hello-world is a dependency of the VFS static filesystem (F_HELLO node), but
# Cargo does not know about this file-level dependency.  We resolve it by
# compiling hello-world explicitly before the rest of the workspace.
echo "[build] Compiling hello-world demo binary..."
RUSTFLAGS="${SERVERS_FLAGS}" cargo build --manifest-path "$ROOT/servers/Cargo.toml" --target x86_64-unknown-none -p hello-world

# Step 1b: Build all remaining ring-3 server ELF binaries (x86_64-unknown-none).
# The kernel embeds these at compile time via include_bytes!(), so they
# must exist before the kernel is compiled.
echo "[build] Compiling ring-3 servers (SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH})..."
RUSTFLAGS="${SERVERS_FLAGS}" cargo build --manifest-path "$ROOT/servers/Cargo.toml" --target x86_64-unknown-none

# Step 2: Build the kernel (x86_64-unknown-uefi).
# Passes extra args (e.g. --release or --features safety-mode) verbatim.
# The kernel's .cargo/config.toml adds -Z stack-protector=strong; repeat it
# here since RUSTFLAGS overrides the config-file flags.
echo "[build] Compiling kernel..."
RUSTFLAGS="${REMAP} -Z stack-protector=strong" cargo build --manifest-path "$ROOT/Cargo.toml" --target x86_64-unknown-uefi "$@"

mkdir -p "$ROOT/build/efi/boot"
cp "$ROOT/target/x86_64-unknown-uefi/debug/Rost.efi" "$ROOT/build/efi/boot/bootx64.efi"
echo "Deployed → build/efi/boot/bootx64.efi"
echo "SHA-256:  $(sha256sum "$ROOT/build/efi/boot/bootx64.efi" 2>/dev/null || shasum -a256 "$ROOT/build/efi/boot/bootx64.efi" | cut -d' ' -f1)"
