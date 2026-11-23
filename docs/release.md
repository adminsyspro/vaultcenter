# Publishing a version

A version is a Git tag `vX.Y.Z`, and a pre-release a tag `vX.Y.Z-rc.N`.
The `release.sh` script prepares the changelog and the version files, then the
CI builds and publishes the image.

## Before the version: keep the changelog up to date

Each PR adds its lines under `## [Unreleased]` in
[CHANGELOG.md](../CHANGELOG.md), in `### Added`, `### Changed` or
`### Fixed`, one `- ` line per visible change.

## Running the release

From the repository root:

```bash
./release.sh X.Y.Z
```

For a pre-release, to install on a test system before the final version:

```bash
./release.sh X.Y.Z-rc.N
```

A pre-release writes its number in the version files but does not touch the
changelog: its changes stay under `## [Unreleased]` until version `X.Y.Z`,
which dates them. It moves neither `latest` nor the `X.Y` and `X` tags (see
below).

The script refuses to continue if any of these conditions is not met (the
message it prints is shown after each condition):

- the number has the strict form `X.Y.Z` or `X.Y.Z-rc.N` (no leading zero,
  no `v`, `N` starting at 1, no other suffix):
  `release.sh: 'X.Y.Z' is not of the form X.Y.Z or X.Y.Z-rc.N`;
- the current branch is `main` and the working tree is clean, untracked files
  included: `release.sh: current branch is not main`,
  `release.sh: working tree has modifications:` followed by the modified files;
- `HEAD` is identical to `origin/main` (after a `git fetch`):
  `release.sh: HEAD differs from origin/main`, or
  `release.sh: git fetch origin failed`;
- the tag exists neither locally nor on `origin`:
  `release.sh: tag vX.Y.Z already exists locally`,
  `release.sh: tag vX.Y.Z already exists on origin`;
- the number is greater than the highest existing `v*` tag, pre-releases
  included, knowing that `X.Y.Z-rc.N` comes before `X.Y.Z`: after
  `v0.2.0-rc.1`, you can publish `0.2.0-rc.2` or `0.2.0`, but no longer
  `0.2.0-rc.1`; after `v0.2.0`, no pre-release of `0.2.0` any more:
  `release.sh: X.Y.Z is not greater than the latest tag vA.B.C`;
- `## [Unreleased]` contains at least one `- ` line:
  `release.sh: CHANGELOG.md has no non-empty '## [Unreleased]' section`.

It then dates the changelog section, writes the version in `Cargo.toml`,
`Cargo.lock`, `ui/package.json` and `ui/package-lock.json`, checks that they
agree with `deploy/tests/check_release_version.sh vX.Y.Z`, creates the commit
`Release vX.Y.Z` and the annotated tag `vX.Y.Z`. A failure before the tag is
created restores the repository to its initial state
(`release.sh: failed, repository restored to <commit>`).

Review the commit and the tag, then answer the question:

```text
Push main and vX.Y.Z? [y/N]
```

- `y` pushes `main`, then the tag.
- If the push of `main` fails, nothing is published: cancel with the command
  below or run both `git push origin` commands again.
- If `main` is pushed but not the tag, do not cancel anything: run
  `git push origin vX.Y.Z`.
- Any other answer pushes nothing. To cancel:
  `git tag -d vX.Y.Z && git reset --hard HEAD~1`.

Once pushed, do not move or delete the tag: publish a later version.

## What the CI publishes

Image: `ghcr.io/adminsyspro/vaultcenter` (public package: everything published
there can be downloaded by anyone).

| Event | Published tags |
| --- | --- |
| tag `vX.Y.Z` | `X.Y.Z`, `X.Y`, `X`, `latest`, `sha-<full SHA>` |
| tag `vX.Y.Z-rc.N` | `X.Y.Z-rc.N`, `sha-<full SHA>` |
| push to `main`, PR, manual run | nothing (build and tests only) |

Publication also requires the repository variable `PUBLISH_IMAGE` set to
`true` (Settings → Secrets and variables → Actions → Variables). Only the
repository that publishes the image has it; a copy of the code elsewhere builds
and tests a tag but publishes nothing.

On a tag, the CI first checks that the tag matches the version in
`Cargo.toml` and, for a final version, that the changelog contains the
section for that version. The smoke test checks that `/health` returns the
expected version and revision before publication. Follow the run in the
GitHub Actions tab.

The version is visible in `/health` and `/v1/status` (`version`, `revision`)
and in the account menu: `VaultCenter X.Y.Z · <7-character revision>`.

## Pulling the image

The package is public: no credentials are needed.

1. Pull the desired version and note its digest:

   ```bash
   docker pull ghcr.io/adminsyspro/vaultcenter:X.Y.Z
   docker inspect --format '{{index .RepoDigests 0}}' ghcr.io/adminsyspro/vaultcenter:X.Y.Z
   ```

2. Deploy that digest, never a moving tag (`latest`, `X.Y`, `X`):

   ```bash
   export VAULTCENTER_IMAGE=ghcr.io/adminsyspro/vaultcenter@sha256:<noted digest>
   ```

After the startup described in the [Docker guide](docker.md), check the
version served (the certificate may not match `localhost`, hence `-k` on the
local machine):

```bash
curl -fsSk https://localhost:8443/health
```

In the JSON response, the `version` field is `X.Y.Z`.
