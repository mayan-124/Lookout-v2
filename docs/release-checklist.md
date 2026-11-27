# Release checklist (steps done by hand)

## Once, before the first release

1. **Back up the signing key.** Keep the updater private key file and its password in two places outside this repository and this PC (for example a password manager and an offline drive). It must be the pair of `plugins.updater.pubkey` in `ui/src-tauri/tauri.conf.json`. If it is lost, installed copies can never be updated.
2. **Add two repository secrets** (GitHub: Settings, Secrets and variables, Actions): `TAURI_SIGNING_PRIVATE_KEY` (the contents of the key file) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.

## Each release

3. Set the same version in `Cargo.toml` (`[workspace.package]`), `ui/src-tauri/tauri.conf.json` and `ui/package.json`. Run `cargo build` and `npm install` in `ui` so both lock files follow. Commit and merge.
4. **Draft release.** Push the tag `v<version>`. The "Release build" workflow creates a draft release with the installer, its `.sig` and `latest.json`. It stops if the release already has assets: delete the draft and run it again.
5. Check that the draft has all three files. Do not publish yet.
6. **Clean install test**, on a PC or Windows Sandbox that never had Lookout, with the installer from the draft:
   - It installs without administrator rights, starts, and finds files.
   - Turn on the add-ons and try each. Without the Visual C++ runtime, "By meaning" shows one plain line and everything else works.
   - Take a screenshot, then uninstall. The screenshot is still there; the registry has no `Lookout` value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` and no key `HKCU\Software\Classes\AppUserModelId\dev.lookout.app`.
7. Publish the draft as the latest release (not a pre-release).
8. **Update test.** On a PC with the previous version installed: Settings, **Check for updates**, then **Download and install**. The new version starts, with settings and data kept. The first release has nothing to update from, so run this test with the second one before telling people about it. If it fails, delete the release so no one else gets it.
