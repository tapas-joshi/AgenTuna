# Developer entry points. The Cargo workspace is the source of truth for checks.

.PHONY: bundle-app bundle-app-universal sign dmg notarize install-linux

# Build and validate target/release/AgenTuna.app for the host architecture.
bundle-app:
	./scripts/bundle.sh

# Build one universal (arm64 + x86_64) app bundle.
bundle-app-universal:
	./scripts/bundle.sh --universal

# Sign the bundle: Developer ID Application with SIGNING_IDENTITY and TEAM_ID, ad-hoc
# otherwise (a local build then reports `Local build (unsigned)`).
sign: bundle-app
	./scripts/sign.sh

# Package the signed host bundle into dist/AgenTuna-<version>-<arch>.dmg with its checksum.
dmg: sign
	./scripts/dmg.sh

# Notarize and staple the app and the dmg (credentials only from the environment).
notarize: dmg
	./scripts/notarize.sh

# Install the Linux desktop entry, hicolor icons and binary under the XDG directories.
# Pass --uninstall to remove them: ./scripts/install-linux.sh --uninstall
install-linux:
	./scripts/install-linux.sh
