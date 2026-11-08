# Releasing AgenTuna

The release pipeline is [`.github/workflows/release.yml`](../.github/workflows/release.yml). It
builds the artifacts AgenTuna actually ships today — macOS arm64 and Linux x86_64 — and on a tag
push creates a **draft** GitHub Release for a maintainer to review and publish. No release has
been published yet.

Windows is not built: there is no Windows job and no Windows bundling script. Do not list a
Windows download anywhere until one exists.

## Artifacts

| File | What it is |
| --- | --- |
| `AgenTuna-<version>-macos-arm64.dmg` | The app bundle, drag-to-Applications dmg. Ad-hoc signed unless the Apple secrets are set. |
| `AgenTuna-<version>-macos-arm64.app.zip` | The same bundle as a zip (`ditto -c -k --keepParent`), the form the notary service and a manual install use. |
| `agentuna-cli-<version>-macos-arm64.tar.gz` | The `agentuna` CLI alone, with `LICENSE`. |
| `AgenTuna-<version>-linux-x86_64.tar.gz` | Portable tree: `target/release/agentuna-app`, `target/release/agentuna`, `crates/at-app/icons/*`, `scripts/install-linux.sh`, the generated desktop entry and `LICENSE`. Extract it anywhere and run `scripts/install-linux.sh`; it mirrors the layout that script expects, so no arguments are needed. |
| `AgenTuna-<version>-linux-x86_64.deb` | Debian package from the Tauri bundler. Best effort — see *Known limits*. |
| `AgenTuna-<version>-linux-x86_64.AppImage` | AppImage from the Tauri bundler. Best effort — see *Known limits*. |
| `agentuna-cli-<version>-linux-x86_64.tar.gz` | The CLI alone, with `LICENSE`. |
| `SHA256SUMS` | `sha256sum` output for every file above. Verify with `sha256sum -c SHA256SUMS` (Linux) or `shasum -a 256 -c SHA256SUMS` (macOS). |

Every file name carries the version and the target, and workflow artifacts are kept for 14 days.

## Cutting a release

1. Bump the version in two places: `[workspace.package] version` in `Cargo.toml` and
   `version` in `ui/package.json`. They must be equal.
   `crates/at-app/tauri.conf.json` declares no `version`, so Tauri derives the bundle version
   from the crate version (which inherits the workspace one). If a literal `version` is ever
   added there, it must match too.
2. Land the bump on `main` with the `rust`, `frontend` and `supply-chain` checks green. The
   release workflow does **not** repeat fmt, clippy, tests or docs; `ci.yml` owns them.
3. Tag and push the tag:

   ```sh
   git tag -a v0.1.0 -m "AgenTuna 0.1.0"
   git push origin v0.1.0
   ```

   A prerelease tag (`v0.2.0-rc.1`) works the same way: set the same string in `Cargo.toml`
   and `ui/package.json`, and the draft is marked as a GitHub pre-release. `CFBundleVersion`
   stays numeric, so `0.2.0-rc.1` and `0.2.0` share one build number.

4. Watch the **Release** workflow. `verify` fails when the tag and the declared versions
   disagree, which is the only version check the pipeline performs.
5. Review the draft, then publish it (see below).

## What the workflow does

```text
verify ──┬─► build-macos-arm64 ──┐
         └─► build-linux-x86_64 ─┴─► publish (draft release, tags only)
```

- **verify** — reads the version from `Cargo.toml`, compares `ui/package.json` and any
  `version` in `tauri.conf.json`, and on a tag push requires the tag to be exactly `v<version>`.
  Fails otherwise. On a manual dry run the same comparison only warns.
- **build-macos-arm64** (`macos-15`) — installs the pinned toolchain and UI dependencies, runs
  `scripts/bundle.sh` (which builds the frontend the app embeds and validates the bundle),
  signs it with `scripts/sign.sh`, packages `scripts/dmg.sh`, notarizes with
  `scripts/notarize.sh` when the Apple secrets exist, then produces the `.app.zip` and the CLI
  tarball. Without secrets the app is ad-hoc signed and there is no notarization: the job works,
  the download is not notarized.
- **build-linux-x86_64** (`ubuntu-22.04`) — installs `libwebkit2gtk-4.1-dev`, `libgtk-3-dev`,
  `libayatana-appindicator3-dev`, `librsvg2-dev`, `libssl-dev`, `patchelf`, then runs
  `scripts/bundle-linux.sh` to produce the binaries and both tarballs. It then runs
  `scripts/bundle-linux.sh --bundler-only` for the `.deb` and `.AppImage` through the Tauri
  bundler, using `crates/at-app/tauri.linux.conf.json`. That file lists only the Linux targets
  (`deb`, `appimage`) so the checked-in `["app"]` macOS target keeps working, adds the package
  descriptions the `.deb` needs, and replaces `build.beforeBuildCommand` with a no-op (`true`)
  because `scripts/bundle-linux.sh` has already built `ui/dist`. That step is
  `continue-on-error`: when the bundler fails the tarballs are still a complete release, and the
  step that follows reports which packages were produced.
- **publish** — runs only on a push of a tag. It downloads both artifact sets, refuses to
  continue if any expected file is missing, writes `SHA256SUMS` (bare names, sorted by file), and
  creates the release with `gh release create --draft --verify-tag --generate-notes`, adding
  `--prerelease` when the tag has a prerelease suffix. The job is the only one with
  `contents: write` (plus `actions: read` for the artifact download); every other job has
  `contents: read`.

## Dry runs

1. Actions → **Release** → **Run workflow**.
2. `dry_run` is `true` by default. A dry run builds every artifact and uploads them to the
   workflow run; it never creates a release. A dry run is also the only way to exercise the
   jobs on a branch or on `main`.
3. Download the artifacts from the run to inspect them.

Do not set `dry_run` to false to "test the publish job": only a push of a tag publishes. The
`publish` job requires `github.event_name == 'push'` with a `refs/tags/` ref, so a dispatch —
even one started on a tag — stays a dry run, and `dry_run=false` on a dispatch is ignored with
a warning. The only way to create a draft is to push a tag, so tag carefully.

## Publishing the draft

The workflow cannot publish. Open the draft release, check that the files and the generated
notes are right, install the dmg or the tarball once, then publish from the Releases page or
with:

```sh
gh release edit v0.1.0 --draft=false
```

Drafts are invisible to watchers; publishing notifies them once, so do not publish to "check
the page".

## Secrets for signing, notarization and the updater

Configure under **Settings → Secrets and variables → Actions**. The macOS job passes each one to
the existing scripts; nothing else reads them, none is echoed, and the notarization password is
redacted by `scripts/notarize.sh` when it prints its plan.

| Secret | Used as | Effect |
| --- | --- | --- |
| `APPLE_CERT_P12` | decoded by the `Import the Developer ID certificate` step | Base64-encoded Developer ID Application certificate + private key (`.p12`), imported into a temporary keychain that exists only for the job. Present or absent is the switch for real signing and notarization. |
| `APPLE_CERT_PASSWORD` | `security import -P` | Password of that `.p12`. |
| `APPLE_SIGNING_IDENTITY` | `SIGNING_IDENTITY` (sign.sh), `AGENTUNA_SIGN_IDENTITY` (dmg.sh) | The exact identity name, for example `Developer ID Application: Your Name (TEAMID1234)`. |
| `APPLE_TEAM_ID` | `TEAM_ID` (sign.sh), `NOTARY_TEAM_ID` (notarize.sh) | Apple Developer team identifier. |
| `APPLE_ID` | `NOTARY_APPLE_ID` | Apple ID that submits the notarization. |
| `APPLE_ID_PASSWORD` | `NOTARY_PASSWORD` | An app-specific password for that Apple ID, not the account password. |
| `TAURI_SIGNING_PRIVATE_KEY` | reserved | Updater signing key. **Unused today**: no update artifact or manifest is produced (see *Known limits*). |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | reserved | Password for the above. Unused today. |

Signing is all-or-nothing by secret presence: with `APPLE_CERT_P12` unset the job runs
`scripts/sign.sh --adhoc`, skips notarization, and says so as a workflow warning.

The job wires the Apple-ID notarization method. `scripts/notarize.sh` also accepts
`NOTARY_PROFILE` (a `notarytool` keychain profile) or `NOTARY_KEY_PATH` + `NOTARY_KEY_ID` +
`NOTARY_ISSUER_ID` (an App Store Connect API key); using either means adding those to the job's
`env:` and storing the key material as a secret.

## Known limits

- **Unsigned builds trigger Gatekeeper.** A dmg built without the Apple secrets carries an
  ad-hoc signature, so macOS blocks the first launch of a downloaded copy. On macOS 15 and later
  the user must open **System Settings > Privacy & Security** and click **Open Anyway** (the
  right-click → **Open** override only works on macOS 13 and 14), or run
  `xattr -dr com.apple.quarantine /Applications/AgenTuna.app`. Only a Developer ID signed and
  notarized build avoids that; it requires the paid Apple Developer Program membership.
- **The Linux GUI is unverified.** No CI job has built the Linux app before: `ci.yml` compiles
  the workspace on macOS and only `at-store` on Linux, so the release workflow is the first Linux
  build. Nothing starts the window, and the `.deb`/`.AppImage` from the Tauri bundler have never
  been installed and launched. The portable tarball plus `scripts/install-linux.sh` is the
  supported path, and release notes must describe the Linux build as untested.
- **The `.deb` and `.AppImage` are best effort.** They need the Tauri bundler to download its
  AppImage tooling on the runner. That step is allowed to fail, and then only the tarballs are
  published.
- **No updater artifacts.** `crates/at-app/src/app/updater.rs` verifies an Ed25519-signed
  manifest against the public key in `crates/at-app/updater-pubkey.txt`, and the manifest URL is
  compiled in through `AGENTUNA_UPDATE_MANIFEST_URL`. Nothing generates or signs that manifest
  yet, so releases carry no update archive, no `latest.json`-style manifest and no
  `TAURI_SIGNING_PRIVATE_KEY` signature. The updater stays inert until a story adds a manifest
  producer and a build-time manifest URL.
- **No SBOM or third-party notice.** The repository has no SBOM or `THIRD_PARTY` generator, and
  the workflow adds no dependency to create one. `LICENSE` ships inside every tarball. When such
  a tool lands, run it in the publish job and put its output in `dist/` so it is attached with
  the rest.
- **macOS arm64 only.** There is no Intel macOS or universal build in the pipeline, even though
  `scripts/bundle.sh --universal` can produce one locally.

## Reproducing a build locally

```sh
# macOS: bundle, sign, package (needs no credentials; the result is ad-hoc signed)
scripts/bundle.sh && scripts/sign.sh --adhoc && scripts/dmg.sh

# macOS with a Developer ID identity in the environment, then notarization
SIGNING_IDENTITY="Developer ID Application: …" TEAM_ID=… scripts/sign.sh
AGENTUNA_SIGN_IDENTITY="Developer ID Application: …" scripts/dmg.sh
NOTARY_APPLE_ID=… NOTARY_PASSWORD=… NOTARY_TEAM_ID=… scripts/notarize.sh

# Linux: portable tarballs, then the Tauri packages
scripts/bundle-linux.sh
scripts/bundle-linux.sh --bundler

# Print the plan without building anything (works on any host)
scripts/bundle.sh --dry-run
scripts/bundle-linux.sh --dry-run
```

Everything is written to `dist/` (git-ignored), and the version always comes from
`[workspace.package] version` in `Cargo.toml`.
