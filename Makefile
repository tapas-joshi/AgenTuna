# Developer entry points. The Cargo workspace is the source of truth for checks.

.PHONY: bundle-app bundle-app-universal dmg

# Build and validate target/release/AgenTuna.app for the host architecture.
bundle-app:
	./scripts/bundle.sh

# Build one universal (arm64 + x86_64) app bundle.
bundle-app-universal:
	./scripts/bundle.sh --universal

# Package the signed host bundle into dist/AgenTuna-<version>-<arch>.dmg with its checksum.
# Requires the bundle to be signed (TF-031); refuses an unsigned bundle.
dmg: bundle-app
	./scripts/dmg.sh
