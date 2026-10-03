# Releasing

1. Update the version in `app/pubspec.yaml` (`x.y.z+build`; the build number
   must grow every release) and in
   `packaging/linux/com.dhivalabs.kilonova.metainfo.xml`.
2. Write `docs/release-notes/vX.Y.Z.md`, add a line to `CHANGELOG.md`, and
   add `fastlane/metadata/android/en-US/changelogs/<build>.txt`.
3. Run `tools/check.sh --regtest` with the devnet up.
4. Tag and push: `git tag -s vX.Y.Z -m vX.Y.Z && git push upstream vX.Y.Z`.
5. The `Release` workflow builds every platform and opens a **draft**
   pre-release with the files and `SHA256SUMS`. Install at least the APK and
   one desktop build from the draft, then publish it.

## Android signing key

Release APKs are signed with one key for the life of the app; Android refuses
updates signed with any other. The workflow reads it from repository secrets
and fails rather than publish an APK signed with something else:

| Secret | Value |
|---|---|
| `KN_KEYSTORE_BASE64` | `base64 -w0 release.jks` |
| `KN_KEYSTORE_PASSWORD` | keystore password |
| `KN_KEY_ALIAS` | key alias |
| `KN_KEY_PASSWORD` | key password |

Create the key once, keep an offline backup, and never commit it. The
keystore is PKCS12, so the key password and the store password are the
same value; set both secrets to it:

```sh
keytool -genkeypair -v -keystore release.jks -alias kilonova \
  -keyalg RSA -keysize 4096 -validity 10000 -dname "CN=Dhiva Labs"
```

Publish the certificate's SHA-256 fingerprint in the release notes so users
can check it (`apksigner verify --print-certs <apk>`).

## Snap Store

`snap/snapcraft.yaml` repackages the release's Linux tarball, so the snap
and GitHub ship the same binary. After the GitHub release is published:

1. In `snap/snapcraft.yaml`, set `version`, the tarball URL and its
   `source-checksum` (from the release's `SHA256SUMS`).
2. Build and upload:

   ```sh
   snapcraft pack --use-lxd
   snapcraft upload --release=stable kilonova_X.Y.Z_amd64.snap
   ```

New cryptocurrency wallet snaps go through manual review at the store
before they appear, so the first upload waited for a reviewer. While a
revision waits in review, later uploads of the same snap cannot be
released; check `snapcraft revisions kilonova` and `snapcraft status
kilonova` before assuming an upload shipped.

## Reproducibility verification

The Linux tarball and the Android APK are built to be reproducible: anyone
who checks out the tag should get byte-identical output. What makes this
work, all inside `.github/workflows/release.yml`:

- `SOURCE_DATE_EPOCH` is taken from the tagged commit's own timestamp
  (`git log -1 --format=%ct`), not "now", and used for the tarball's `tar
  --mtime` and the `.deb`'s staged file times.
- The Linux tarball is built with `--sort=name --owner=0 --group=0
  --numeric-owner` and piped through `gzip -n` so member order, ownership
  and the gzip header are all fixed.
- `RUSTFLAGS` (plain, for the Linux and Windows jobs) or
  `CARGO_ENCODED_RUSTFLAGS` (for Android, since cargokit's own NDK setup
  already uses that form and cargo refuses both being set at once) carries
  `--remap-path-prefix` for both the checkout path and `CARGO_HOME`
  (pinned to a path under the workspace rather than left at its
  machine-dependent default), so the embedded path in `libkn_ffi.so` /
  `kn_ffi.dll`'s debug info and panic strings is the same regardless of
  which runner or directory built it.
- `CARGO_INCREMENTAL=0` throughout, since incremental compilation caches
  are keyed by absolute paths.

Checking it:

- **Locally, Linux only:** `tools/repro-check.sh` builds the Linux release
  bundle twice, from two different temporary paths, and diffs the
  resulting tarballs (byte-for-byte if they match; with `diffoscope` if
  it's installed, otherwise a per-file sha256 comparison, if they don't).
  Needs about 6 GB of free disk; it cleans up its own git worktrees and
  build output on exit either way.
- **In CI, both platforms:** `.github/workflows/reproducible.yml` builds
  the Linux tarball and the arm64 APK on two separate runners (a real
  cross-machine test, not just cross-directory) and fails if the two
  runners' output differs. It runs on tags and by hand, and reports rather
  than blocking `release.yml`, since a newly-discovered nondeterminism
  shouldn't itself hold up a release. The Android comparison signs both
  runners' APKs with the same throwaway, freshly-generated-every-run key
  (not the real release key) so a signing-key difference can't be mistaken
  for a build difference; see the `keystore` job's comment in that
  workflow.

**Known gap:** the fix above covers `kn_ffi`, the Rust library (confirmed
with `tools/repro-check.sh`: it was the only shared library byte-identical
between two builds from different paths before this work, and nothing
else needed to change for it). The rest of the Linux bundle, built by
Flutter's own tooling, is not yet reproducible:

- `kilonova` (the GTK runner binary) and the bundled Flutter plugins'
  `.so` files are compiled from Flutter-generated C++ by CMake/clang,
  which embeds the absolute build path the same way rustc does, but
  through `__FILE__`/debug info rather than `file!()`. The fix is the C
  compiler's equivalent of `--remap-path-prefix`:
  `-ffile-prefix-map=$PWD=/build` added to `CMAKE_C_FLAGS`/
  `CMAKE_CXX_FLAGS` in `app/linux/CMakeLists.txt` (and the matching
  per-plugin CMake files cargokit/Flutter generate under
  `app/linux/flutter/ephemeral/`, if those turn out to matter once the
  top-level flags are in place). This is an `app/` file, outside what
  this change touches; the person working on `app/` should make it.
- `libapp.so` is the Dart AOT snapshot of `app/lib`. Whether its bytes are
  path-independent depends on the Dart compiler's own snapshot format and
  is not something a build-script flag fixes; needs investigation on the
  `app/` side (likely starting from Flutter's `--split-debug-info` and
  `--dart-define` handling, or an upstream Flutter issue if the snapshot
  itself embeds the absolute source path).
- The Windows `.zip` and the Inno Setup installer are not reproducible at
  all yet: `Compress-Archive` and Inno Setup both embed file timestamps in
  their output. `RUSTFLAGS` is set for the Windows job too, so `kn_ffi.dll`
  itself has the same property as the Linux `.so`, but that is all this
  change gets on Windows.
- The `.deb`: `build-deb.sh` now accepts `SOURCE_DATE_EPOCH` and pins
  staged file mtimes to it, and `dpkg-deb` itself honors
  `SOURCE_DATE_EPOCH` for the archive's own timestamps, but this hasn't
  been verified end to end with two real builds the way the tarball has.

## Flatpak

`packaging/flatpak/com.dhivalabs.kilonova.yml` repackages the release's
Linux tarball (`type: archive`, with the URL and sha256 from that
release's `SHA256SUMS`), the same way the Snap does, rather than building
from source inside the Flatpak sandbox. Update its `url` and `sha256`
after each GitHub release, the same step as the Snap's `source-checksum`.
`.github/workflows/flatpak.yml` builds the manifest (not a Flathub
submission) on any PR touching `packaging/flatpak/`, to catch a broken
manifest or a stale checksum early.

Submitting to Flathub is a one-time fork-and-PR against
`flathub/flathub`, then routine version-bump PRs against the
`flathub/com.dhivalabs.kilonova` repository Flathub creates once that's
accepted. Full steps, including what a reviewer tends to check, are in
`packaging/flatpak/README.md`.

## F-Droid

`packaging/fdroid/com.dhivalabs.kilonova.yml` is a draft of the recipe
that goes into fdroiddata (F-Droid's own metadata repository; nothing in
this repository reads this file directly). Unlike the Snap and Flatpak,
F-Droid builds from source on its own server, so the recipe has to install
the pinned Rust toolchain and run `cargo`/`flutter build apk` by hand
(`sudo`/`prebuild`/`build` steps) rather than just fetching a release
tarball; see the recipe's own comments and `packaging/fdroid/README.md`
for the submission steps, how to test it locally with `fdroid build`
first, and the open question of whether to ask F-Droid for the same
split-per-ABI APKs the GitHub release ships instead of one universal APK.

Submitting needs a `gitlab.com` account (fdroiddata is hosted there) and a
merge request against `fdroiddata`; after that, `UpdateCheckMode: Tags`
lets F-Droid's own bot propose the version-bump MR for most releases.

## Windows code signing

`release.yml`'s `windows` job has an optional signing step: when
`KN_WIN_CERT_BASE64` is present in repository secrets, it signs
`kilonova.exe` and `kn_ffi.dll` before zipping, and the installer `.exe`
after Inno Setup builds it, both with `signtool /fd SHA256 /tr
http://timestamp.digicert.com /td SHA256`. When the secret is absent, the
step records that and skips signing entirely; the zip and installer ship
unsigned, as they do today, and SmartScreen keeps warning until a
certificate is added.

**Certificate options.**

- **OV (Organization Validation) vs EV (Extended Validation).** Both
  identify the publisher the same way to signtool; the difference used to
  be that EV got SmartScreen's reputation bar to drop immediately while OV
  had to build reputation over time from download volume. As of the
  CA/Browser Forum's 2023 baseline requirements, private keys for both OV
  and EV code-signing certificates must live in a hardware token or an
  equivalent HSM; the all-software "just a .pfx file" certificate is gone
  for either tier.
- **Cloud HSM signing services.** Because the key must be in an HSM now,
  most issuers sell access to one rather than shipping a USB token:
  Azure Trusted Signing (Microsoft's own, integrates with `signtool`
  directly) and SSL.com's eSigner are the two most commonly mentioned for
  this. Evaluate current pricing and jurisdiction requirements directly
  with the vendor; nothing here should be taken as a quote, since prices
  and terms move and weren't independently verified for this change.
- **A physical hardware token**, shipped by the CA, is still offered by
  some issuers as an alternative to a cloud HSM and plugs into a build
  machine (or a machine someone runs `signtool` on by hand, copying the
  artifacts there) rather than CI directly, since GitHub Actions runners
  can't have a USB device attached.

**Key custody.** Whichever option is chosen, write down here, before
buying anything: who holds the credentials to the HSM/signing service (one
named person, not a shared login), where the recovery/backup credentials
for that account live, and what happens if that person is unavailable.
For a cloud HSM service this is an account password plus MFA plus
whatever API credential signtool or the CI job needs; for a physical
token, a PIN and the token itself, kept somewhere that survives one
person's laptop dying.

**Adding the secrets.** Once a certificate exists:

| Secret | Value |
|---|---|
| `KN_WIN_CERT_BASE64` | `base64 -w0 your-cert.pfx` (or the export process your HSM/signing service's tooling gives you) |
| `KN_WIN_CERT_PASSWORD` | the certificate's password/PIN |

Add both under the repository's Settings → Secrets and variables →
Actions, the same place the Android keystore secrets live. The next
tagged release signs automatically; nothing else changes.
