# F-Droid

`com.dhivalabs.kilonova.yml` is a draft of the recipe that goes into
fdroiddata (F-Droid's metadata lives in its own repository, separate from
this one). It is not built or checked by anything here; keep it up to date
by hand on each release and copy it over when submitting.

## Before submitting

- **Account.** Submitting needs a GitLab account on `gitlab.com` (fdroiddata
  is hosted there) belonging to whoever submits. This repository's tooling
  does not create one.
- **Package ID match.** `app/android/app/build.gradle.kts`'s
  `applicationId` and every `fastlane/metadata/android/en-US/` file use
  `com.dhivalabs.kilonova`; the recipe's filename and `AutoName` must match
  that exactly, and do.
- **A real build attempt.** F-Droid's reviewers expect the recipe to have
  been tried, not just written. `fdroid build` (from the `fdroidserver`
  tools) can run it locally before submitting; see "Testing the recipe"
  below.
- **The ABI split question.** The GitHub release and the Snap/Flatpak ship
  split-per-ABI APKs (`flutter build apk --split-per-abi`); the draft
  recipe instead builds one universal APK, since an F-Droid `Build` entry
  produces exactly one output file. F-Droid does support multiple
  `Builds` entries at the same `versionName` with different
  `versionCode`s and an `gradle:`/`output:` pointed at a specific ABI's
  split, which would make Kilonova's F-Droid APKs smaller the way the
  GitHub ones are. Decide this with an F-Droid reviewer rather than
  guessing; the universal APK is the simpler starting point.

## Testing the recipe

```sh
pip install --user fdroidserver   # or your distro's package
mkdir fdroidtest && cd fdroidtest
fdroid init  # creates a scratch fdroiddata-shaped directory
cp /path/to/kilonova/packaging/fdroid/com.dhivalabs.kilonova.yml metadata/
fdroid build -v -l com.dhivalabs.kilonova
```

`fdroid build` runs the recipe's `sudo`/`srclibs`/`prebuild`/`build` steps
inside F-Droid's own build container (`buildserver`), which is the closest
local approximation of F-Droid's actual build server. Expect to iterate on
the Rust toolchain install and the NDK version pin
(`ndk: 27.0.12077973` in the recipe; check it still matches a version
F-Droid's build server has, and that it satisfies
`app/android/app/build.gradle.kts`'s `flutter.ndkVersion`).

## Submitting

1. Fork `https://gitlab.com/fdroid/fdroiddata`.
2. Branch from `master`, name it something like
   `com.dhivalabs.kilonova` (F-Droid's contributing guide calls this the
   "new app" branch naming; any descriptive name is accepted).
3. Copy `com.dhivalabs.kilonova.yml` from this directory into
   `metadata/com.dhivalabs.kilonova.yml` in that branch, with the current
   release's `commit:` (a tag, e.g. `v0.3.0`) and matching
   `versionName`/`versionCode`.
4. Open a merge request against `fdroiddata`. The CI there runs
   `fdroid build` and `fdroid rewritemeta --list` checks automatically;
   fix whatever it reports rather than arguing with the bot.
5. A human reviewer looks at `ScanIgnore`/`scandelete` entries especially
   closely (ours covers only test fixtures and fuzz corpus data, listed
   with the reason in the recipe's comments) and at whether the build is
   genuinely reproducible from source, which is the thing F-Droid cares
   about that Flathub and the Snap Store do not: F-Droid's own server
   builds the APK from source and only ships what it built itself, so
   `cargo`'s network fetch of monero-oxide and its git dependencies during
   the build is expected and normal, not a problem to hide.
6. Once merged, every future release needs a follow-up MR updating
   `CurrentVersion`, `CurrentVersionCode` and adding a new `Builds` entry;
   `UpdateCheckMode: Tags` means F-Droid's own bot can often open that MR
   automatically after a tag is pushed, for a maintainer to review.

## Keeping this draft current

Update `packaging/fdroid/com.dhivalabs.kilonova.yml` in the same PR that
tags a release, the same way `snap/snapcraft.yaml` and
`packaging/flatpak/com.dhivalabs.kilonova.yml` are, even though this one
isn't consumed automatically by anything in this repository.
