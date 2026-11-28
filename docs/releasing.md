# Releases

## GitHub Release binaries

Only pushing a matching `v*` tag runs `.github/workflows/release.yml`. Ordinary
pushes, pull requests and manually running the normal CI workflow do not run the
release matrix or publish assets. The tag must match every workspace package and
internal dependency version. The normal CI checks must pass before release builds.
Normal CI checks the complete workspace on Linux and Windows, including the Linux
desktop with GTK/WebKit dependencies, before a release tag is created. It also
builds and mounts an Intel Mac disk image and saves it as a CI artifact for seven days.

| Platform | Native runner | Rust target | Downloads |
| --- | --- | --- | --- |
| macOS ARM64 | `macos-15` | `aarch64-apple-darwin` | `arcnix-vVERSION-macos-arm64.dmg` |
| macOS Intel | `macos-15-intel` | `x86_64-apple-darwin` | `arcnix-vVERSION-macos-amd64.dmg` |
| Windows AMD64 | `windows-2025` | `x86_64-pc-windows-msvc` | `arcnix-vVERSION-windows-amd64-setup.exe` (installer), `arcnix-vVERSION-windows-amd64.zip` (portable) |
| Linux ARM64 | `ubuntu-24.04-arm` | `aarch64-unknown-linux-gnu` | `arcnix-vVERSION-linux-arm64.tar.gz` |
| Linux AMD64 | `ubuntu-24.04` | `x86_64-unknown-linux-gnu` | `arcnix-vVERSION-linux-amd64.tar.gz` |

Each native runner tests the workspace, builds with Rust **1.100.0** in release
mode, and packages the binary, READMEs and licenses. All five builds must succeed
before the downloads and `SHA256SUMS` are attached to a GitHub Release. Tags containing
a prerelease suffix produce a prerelease. Rerunning the release job replaces assets
on the existing release.

`tools/package_release.py` reads Cargo metadata to locate the actual workspace
target directory. It does not change Cargo home, the build directory or TEMP.

macOS packaging uses `hdiutil` to create a compressed, read-only disk image.
Release CI mounts each image on its native architecture and checks the Applications
shortcut, Mach-O architecture, bundle signature and executable version with `tools/macos_dmg_smoke.sh`.

Windows packaging uses Inno Setup. Local packaging requires `ISCC.exe` on `PATH`.
On first install, the wizard offers “Install for me only” (default,
`%LOCALAPPDATA%\Programs\Arcnix`) and “Install for all users” (requires
administrator privileges, `%ProgramFiles%\Arcnix`). Upgrades reuse the existing
install scope. For unattended installs, pass `/CURRENTUSER` or `/ALLUSERS`.
Windows release CI verifies silent installation, the Start menu shortcut,
the installed executable, registration in Windows Settings → Apps and
uninstallation for both scopes. User settings survive uninstall.
For an all-users installation in Program Files, use the installer to update
when the current user lacks write access to the application folder.

The Windows x64 executable carries the pinned LGPL libmpv runtime and its
notices in an appended ZIP. The installer and portable ZIP ship the same executable. Opening an MKV for the first time extracts the runtime
to the user's local application cache. An adjacent `libmpv-2.dll` takes precedence
for development or replacement. To stage it for a developer build, run
`python tools/libmpv_runtime.py <directory-containing-arcnix.exe>`.

## Desktop update channel

Windows x64 updates use the same portable ZIP offered for manual downloads.
The updater selects the exact `arcnix-vVERSION-windows-amd64.zip` asset from
the pinned stable GitHub release and verifies its size and GitHub API SHA-256
digest before opening it. Only `arcnix-vVERSION-windows-amd64/arcnix.exe` is
read into the private update stage. The extracted executable has its own
digest for installation, architecture and version checks, replacement and rollback.
The ZIP and staged binary are revalidated before cache reuse.

Clients through 0.3.1 expect a standalone executable. Install the first
ZIP-aware version manually once using the installer or portable ZIP. Subsequent
updates use the portable ZIP in-app. Historical releases keep their assets.

Only Windows x64 supports automatic replacement. Other platforms expose the
release page. The desktop checks for stable releases while running, starting
after five seconds if a check is due. New configurations check every six hours.
Settings also offers three or twelve hours, daily, weekly and monthly intervals.
Existing configurations retain their saved interval. The loopback fixture
override is compiled into debug builds only.

When a newer release is found, a download icon appears above Settings in the
left navigation rail. On Windows x64 the default is to download and verify it
in the background. Clicking the icon shows the release notes and progress.
Once ready, choose **Restart and install** to confirm, or **Later** to defer.
Automatic download and unattended installation have separate preferences.

Verified files are staged beside the configuration under `updates`. A trusted
copy of the current program validates the plan and acknowledges readiness before
the current process exits. Installation requires a writable application folder,
no SSH connection, no pending transfers or tasks, and no unsaved editor work.
If unattended installation is enabled, a deferrable 30-second countdown starts
when the app is idle. On confirmation or expiry, the helper replaces the executable, preserves `arcnix.exe.previous`,
starts the new program with the same configuration, and requires a startup
acknowledgement within 30 seconds. An early exit or missing acknowledgement
restores/restarts the old program. Failure details survive restart and suppress
automatic retries of that version. Release assets are never executed before
their API digest, architecture and reported version have been checked.

Validate replacement and rollback only on disposable copies:

```powershell
python tools/update_smoke.py
```

The test uses loopback release metadata, compiled test executables and isolated
configuration/WebView profiles. It never replaces the working installation.

## Platform requirements

- **Windows:** MSVC build tools, native Windows Perl and NASM for building;
  Inno Setup for packaging and WebView2 Runtime for running.
- **macOS:** Xcode Command Line Tools for building; macOS 13+ on Apple Silicon or Intel
  for running. Open the DMG and drag `Arcnix.app` to Applications. CI creates an ad-hoc
  signature; the app is not Developer ID signed or notarized.
- **Linux:** native builds on Ubuntu 24.04 with `build-essential`, `pkg-config`,
  `perl`, `libssl-dev`, `libgtk-3-dev` and `libwebkit2gtk-4.1-dev`. Archives contain
  the `arcnix` executable and require a graphical desktop, glibc 2.39+, GTK 3 and
  WebKitGTK 4.1 at runtime. On Ubuntu 24.04, install `libgtk-3-0t64` and
  `libwebkit2gtk-4.1-0`. These are not fully static or AppImage packages.

Native file selection and clipboard integration currently have Windows-only
implementations. Building other platforms does not establish full UI parity.

References: [GitHub runner platforms](https://docs.github.com/en/actions/reference/runners/github-hosted-runners),
[Wry Linux dependencies](https://github.com/tauri-apps/wry#linux-dependencies).

## Publish a version

1. Update `workspace.package.version` and the `arcnix-core` dependency version in
   the root `Cargo.toml`; update `Cargo.lock` and `CHANGELOG.md`.
2. Commit and push the changes to `main` and wait for CI to pass.
3. Create and push a new matching tag, for example:

   ```sh
   git tag v0.2.0
   git push origin v0.2.0
   ```

The example requires first updating the manifests to `0.2.0`. Existing release
tags retain their original workflow and are not moved by CI configuration changes.
No release is triggered by editing these workflows.

Before pushing a tag, validate its version locally:

```sh
python tools/check_release.py v0.2.0
```

Rustfmt uses **nightly-2026-11-04** to honor the project's existing unstable
formatting settings. Compilation, Clippy and tests use **Rust 1.100.0**.

Arcnix distributes new versions through GitHub Releases. Both workspace crates
set `publish = false` in their Cargo manifests.

## 发布说明

- 推送匹配工作区版本的 `v*` tag 后，发布流程在五个平台上运行原生测试、构建和打包。普通 CI 会在 Intel Mac 上构建并验证 DMG，测试产物保留七天。
- macOS 为 Apple Silicon 和 Intel Mac 分别提供 `.dmg`，打开后将 `Arcnix.app` 拖到「应用程序」。Windows 提供 `-setup.exe` 安装器和便携 ZIP，Linux 提供 x64 和 ARM64 的 tar.gz。全部通过后发布 GitHub Release 附件和 `SHA256SUMS`。
- macOS 目前仅临时签名，未经公证；Linux 包依赖系统 GTK 3、WebKitGTK 4.1 和 glibc 2.39+。原生文件选择、剪贴板当前仍以 Windows 为实现平台。
- 新版本通过 GitHub Releases 分发，两个工作区 crate 均已禁用 Cargo 发布。
- 已存在的 tag 不会被移动；本次配置更新不触发发布。新版本先更新版本与锁文件，普通 CI 通过后再推送新 tag。
