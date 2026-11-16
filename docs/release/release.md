# Releasing

How a version of Illumination ships. The model is the user's other project, Straylight
(`docs/dev/release-cycle.md` there): a tag on the bump commit, a GitHub Release on the tag,
an installer as the artifact.

## Versions, tags and releases

- **A version is a commit.** The version lives in `Cargo.toml` (the workspace's, which every
  crate takes) and in `Cargo.lock`, synced by `cargo build`. The commit that bumps it carries
  the dated `CHANGELOG.md` entry, and its message is exactly `vX.Y.Z`, with no trailer.
- **A tag is a release.** Not every version is released. A version is tagged `vX.Y.Z` only
  once it has been confirmed on both desks, and the tag goes **on its bump commit**, so the
  tag and the program's own version always name the same source. Never tag an earlier commit
  and bump later.
- Versions up to v0.12.8 were each tagged as they were made. From v0.13.0 on, a version
  without a tag is one that was not released; its changelog entry says so.

## Building the installer

```powershell
pwsh scripts\build-installer.ps1
```

It makes the release build and then `target\installer\Illumination_X.Y.Z_x64-setup.exe`
with NSIS (`installer\illumination.nsi`). It refuses to package a build whose version is not
the one in `Cargo.toml`. `makensis.exe` is looked for on `PATH`, in NSIS's usual folders and
in the copy Tauri keeps under `%LOCALAPPDATA%\tauri\NSIS`.

What the installer does:
- asks for an administrator once (the service is the reason; Straylight installs per user
  and does not ask, having no service);
- stops the service of an earlier version, and waits for the app's windows to be closed
  rather than closing them;
- copies `illum.exe`, the FFmpeg libraries and the licence to `C:\Program Files\Illumination`;
- adds the Start-menu and desktop shortcuts and the entry in Windows' list of apps;
- runs `illum service install`: registers and starts the service, opens the firewall for the
  program, and lets the app and the service share `%ProgramData%\Illumination`.

Its uninstaller runs `illum service uninstall` and removes what the installer added.
`%ProgramData%\Illumination` (this desk's key, its desks, its settings, the logs) is kept by
both, so an update or a reinstall is the same desk afterwards. `/S` installs and uninstalls
without windows.

It is not code-signed. On the first start Windows SmartScreen says "Windows protected your
PC"; "More info", then "Run anyway".

## Releasing a version

1. Both desks run the version (installed from the installer) and the checks for what changed
   pass.
2. Tag the bump commit: `git tag -a vX.Y.Z -m vX.Y.Z`, `git push origin vX.Y.Z`.
3. Build the installer from that commit.
4. Create the GitHub Release on the tag and attach `Illumination_X.Y.Z_x64-setup.exe`.

## Not built yet

- **Updating itself.** Straylight's installs poll a `latest.json` on the latest release and
  install a signed update with the person's OK (Tauri's updater). Illumination has no such
  thing yet: an update is a download and a run of the new installer. When it is built, the
  release gains the signature and the manifest as artifacts.
- **Building in CI on a tag.** By hand for now, as in Straylight.
