# Flatpak

`com.dhivalabs.kilonova.yml` repackages the release's Linux tarball (the
same one the Snap uses), so Flathub, the Snap Store and GitHub all ship the
same binary. It does not build Kilonova from source inside the sandbox.

## Try it locally

```sh
flatpak-builder --user --install --force-clean build-dir packaging/flatpak/com.dhivalabs.kilonova.yml
flatpak run com.dhivalabs.kilonova
```

Needs `org.gnome.Platform//51` and `org.gnome.Sdk//51` installed
(`flatpak install flathub org.gnome.Platform//51 org.gnome.Sdk//51`).

## Submitting to Flathub

Flathub builds from a manifest in its own repository, not from this one, so
there is a one-time submission step and then routine version-bump PRs. This
needs a Flathub/GitHub account for whoever submits; the steps below prepare
everything up to that point.

1. **Fork `flathub/flathub`** on GitHub (this is the one-time intake
   repository; Flathub then creates a dedicated
   `flathub/com.dhivalabs.kilonova` repository once the submission is
   accepted).
2. Clone the fork, create a branch (Flathub's convention is
   `new-pr/<app-id>`, e.g. `new-pr/com.dhivalabs.kilonova`).
3. Copy `com.dhivalabs.kilonova.yml` from this directory into the root of
   that branch, with the `url`/`sha256` already pointed at the current
   tagged release (keep this file and the one in this repository in sync;
   update both on every release).
4. Open a PR against `flathub/flathub` from that branch. Flathub's bot runs
   a build and basic checks; a human reviewer looks at the manifest
   (permissions especially — ours asks for network, Wayland/X11, IPC and
   DRI only, no filesystem access, which reviewers generally wave through
   quickly) and the metainfo file's completeness.
5. Once accepted, Flathub creates `flathub/com.dhivalabs.kilonova` as its
   own repository and the submission PR's contents become its initial
   commit. Future releases are PRs against that repository: bump `url` and
   `sha256`, and add a `<release>` entry to
   `packaging/linux/com.dhivalabs.kilonova.metainfo.xml` in this repo (the
   file Flathub's copy mirrors) in the same PR that tags the release.
6. Flathub's own build server does the reproducibility check Flathub cares
   about: that *their* build matches what's declared, not that our tarball
   is itself reproducible (that's a separate goal; see
   docs/RELEASING.md#reproducibility and `tools/repro-check.sh`).

## Screenshots

Flathub's review expects at least one screenshot referenced from the
metainfo file's `<screenshots>` section. None are included yet; add
`<screenshot>` entries pointing at hosted images (Flathub accepts URLs, so
these can live on the GitHub repo or a release asset) to
`packaging/linux/com.dhivalabs.kilonova.metainfo.xml` before submitting,
since this is the one thing in the current metainfo file a reviewer is
likely to ask for.

## Validating the manifest and metainfo

```sh
appstreamcli validate packaging/linux/com.dhivalabs.kilonova.metainfo.xml
flatpak run org.flatpak.Builder --help   # or: flatpak-builder --help
```

`.github/workflows/flatpak.yml` runs a build of this manifest (not a
Flathub submission, just a build-and-sanity-check) on pull requests that
touch `packaging/flatpak/`.
