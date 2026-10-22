#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo test --workspace
cargo build -p witchat-app --release
TARGET_DIR="$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
cargo run -q -p witchat-xtask -- dist "$TARGET_DIR/release/witchat" dist/linux
chmod +x dist/linux/witchat
ls -l dist/linux
echo "Portable binary: dist/linux/witchat (verify: cd dist/linux && sha256sum -c witchat.sha256)"
