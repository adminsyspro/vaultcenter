# Changelog

All notable changes to VaultCenter are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- The image ships the isolation receipt of its runtime, from a two-VTL isolation acceptance on a dedicated lab appliance and PBS. Without it, a new installation could not activate any VTL.

## [0.2.4] - 2026-11-24

### Fixed

- A new installation no longer stays stuck at the startup step: the installer now writes the pool of mhVTL resources (`vtl-runtime.json`) that the appliance requires in isolated mode.

## [0.2.3] - 2026-11-24

### Changed

- The sign-in page shows the appliance front panel over a backdrop of light streams, with the logo and a cartridge being written, instead of a plain card.
- In the local demonstration, signing out shows the sign-in page; any credentials sign back in.

## [0.2.2] - 2026-11-23

### Changed

- The installer, the updater, the API, the controllers and the documentation now speak English. The interface still shows French to French users: it translates the new English messages back, including messages stored by earlier versions.
- The release script asks `Push main and vX.Y.Z? [y/N]`.
- An installation under the former product name is still refused, with a message that no longer points to a migration guide.
- An image is published only by the repository whose `PUBLISH_IMAGE` variable is `true`.

### Fixed

- The development server no longer targets a fixed internal address by default.

## [0.2.1] - 2026-11-23

### Changed

- The installation downloads the public image without credentials: without `--token`, the script no longer calls `vaultcenter.io` and no longer logs in to the registry.
- The CI no longer publishes an image on every push to `main`: only tagged versions are published.

### Fixed

- The interface no longer serves a file that was not meant for publication, and its examples use neutral addresses.

## [0.2.0] - 2026-11-23

### Added

- Command to install and update the appliance on Debian 12 and 13 (`install.sh`), with a backup and automatic rollback on failure.
- The mhVTL kernel module is installed with DKMS, so that a kernel update does not leave the appliance stopped: the headers follow the kernel, the module is built for each installed kernel, and it is rebuilt at boot if it is missing.
- The image now ships the host files (controllers, units, `install.py`, `update.py`) and the installation guide.
- A pre-release (`X.Y.Z-rc.1`) can be installed with `--version`.
- The installation runs without a customer token: `curl -fsSL https://vaultcenter.io/install | sudo bash`. `--token` becomes optional.
- A pre-release (`X.Y.Z-rc.N`) can be published with `release.sh`; its image moves neither `latest` nor the `X.Y` and `X` tags.
- A local storage gauge, in the status bar and on each row of the VTL management page, shows what remains to provision once the space promised to existing cartridges is deducted.
- The library activity log can be cleared.
- The throughput chart offers “All cartridges”: a single curve that adds up all cartridges, shown by default when the drive is empty.

### Fixed

- The host storage service starts before Docker: the appliance comes back after the machine restarts.

## [0.1.0] - 2026-10-23

### Added

- Web console to manage several virtual tape libraries (VTL).
- Export of backups to S3, verified and resumable after an interruption.
- Restore from cartridges and reintegration into Proxmox Backup Server (PBS).
- Archiving and recall of cartridges to OVH Cold Archive.
- S3 bucket rotation.
- Activity bell that reports operations and incidents.
- Docker image to deploy VaultCenter.
