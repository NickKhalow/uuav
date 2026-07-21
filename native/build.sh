#!/bin/bash

set -e # Stop on error

TARGET="x86_64-pc-windows-gnu" # linker configured in .cargo/config.toml
DEST_DIR="../Packages/UUAV/Runtime/Plugins/x86_64"

cargo build --release --target "$TARGET"

mkdir -p "$DEST_DIR"
cp "target/$TARGET/release/uuav.dll" "$DEST_DIR/"

echo "Deployed to: $DEST_DIR"
echo "Make sure to provied `libwinpthread-1.dll` and ffmpeg binaries mentioned in the readme file"
