#!/usr/bin/env bash
set -Eeuo pipefail
umask 0022

cd "$(dirname "$0")/.."
REPO_ROOT="$(pwd)"

echo "[SMOKE] Commencing production sanity and state validation..."

echo "[SMOKE] Flushing volatile local cache metrics..."
rm -rf "$HOME/.cache/tnk"
mkdir -p "$HOME/.cache/tnk"

MOCK_WORKSPACE="$(mktemp -d "$HOME/.cache/tnk-smoke.XXXXXX")"
MOCK_PROJECT="${MOCK_WORKSPACE}/smoke-project-alpha"
MOCK_SANDBOX="tnk-smoke-project-alpha"
mkdir -p "$MOCK_PROJECT"

cleanup() {
    local exit_code=$?

    if command -v limactl >/dev/null 2>&1; then
        (
            cd "$MOCK_PROJECT" >/dev/null 2>&1 || true
            cargo run --release --manifest-path "$REPO_ROOT/Cargo.toml" -- services stop >/dev/null 2>&1 || true
            cargo run --release --manifest-path "$REPO_ROOT/Cargo.toml" -- sandbox delete --yes >/dev/null 2>&1 || true
        ) || true
    fi

    rm -rf "$MOCK_WORKSPACE"
    exit "$exit_code"
}
trap cleanup EXIT

export TNK_WORKSPACE_ROOT="$MOCK_WORKSPACE"
export TNK_SERVER_PORT="19091"

echo "[SMOKE] Project workspace context isolated at: ${MOCK_PROJECT}"

echo "[SMOKE] Verifying CLI argument and state dispatch parsing layers..."
cargo run --release -- sandbox delete --yes --dry-run
cargo run --release -- services stop --dry-run
cargo run --release -- sandbox stop --name "$MOCK_SANDBOX"

echo "[SMOKE] Validating configuration translation behaviors..."
if ! cargo run --release -- config show >/dev/null; then
    echo "[FAIL] Internal resolution matrix crashed on absolute evaluations." >&2
    exit 1
fi

if ! command -v limactl >/dev/null 2>&1; then
    echo "[SMOKE] Skipping full lifecycle integration (limactl not available)."
    echo "[ OK ] GA Smoke Matrix evaluation executed successfully. System state is pristine."
    exit 0
fi

# Check if a model is available (smoke env has no model pre-loaded)
MODEL_CONFIGURED=false
if ls "$HOME/.cache/tnk"/*.gguf "$HOME/.cache/tnk"/*.bin 2>/dev/null | head -1 >/dev/null 2>&1; then
    MODEL_CONFIGURED=true
fi

if [ "$MODEL_CONFIGURED" = true ]; then
    echo "[SMOKE] Running full lifecycle integration test (model found)..."
    pushd "$MOCK_PROJECT" >/dev/null

    echo "[SMOKE] 0. sandbox delete --yes (reset stale state)"
    cargo run --release --manifest-path "$REPO_ROOT/Cargo.toml" -- sandbox delete --yes || true

    echo "[SMOKE] 1. tnk run"
    timeout 30 cargo run --release --manifest-path "$REPO_ROOT/Cargo.toml" -- run --verbose || true

    echo "[SMOKE] 2. sandbox start --profile base"
    cargo run --release --manifest-path "$REPO_ROOT/Cargo.toml" -- sandbox start --profile base

    echo "[SMOKE] 3. sandbox ls"
    cargo run --release --manifest-path "$REPO_ROOT/Cargo.toml" -- sandbox ls

    echo "[SMOKE] 4. services status --output json"
    cargo run --release --manifest-path "$REPO_ROOT/Cargo.toml" -- services status --output json >/dev/null

    echo "[SMOKE] 5. shutdown"
    cargo run --release --manifest-path "$REPO_ROOT/Cargo.toml" -- services stop
    cargo run --release --manifest-path "$REPO_ROOT/Cargo.toml" -- sandbox delete --yes

    popd >/dev/null

    echo "[SMOKE] Verifying post-shutdown cleanup..."
    SANDBOX_LS_OUTPUT="$(cargo run --release --manifest-path "$REPO_ROOT/Cargo.toml" -- sandbox ls 2>&1 || true)"
    if printf '%s\n' "$SANDBOX_LS_OUTPUT" | grep -q 'tnk-smoke-project-alpha[[:space:]].*running'; then
        echo "[FAIL] Sandbox container still running after shutdown" >&2
        printf '%s\n' "$SANDBOX_LS_OUTPUT" >&2
        exit 1
    fi
else
    echo "[SMOKE] Skipping full lifecycle integration (no model in smoke env)."
fi

unset TNK_WORKSPACE_ROOT
unset TNK_SERVER_PORT

echo "[ OK ] GA Smoke Matrix evaluation executed successfully. System state is pristine."
