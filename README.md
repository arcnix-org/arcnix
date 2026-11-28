# Arcnix

[简体中文](README.zh-CN.md) · [Download](https://arcnix.org/#downloads) · [Quick start](https://arcnix.org/docs/quick-start/) · [Documentation](https://arcnix.org/docs/)

Arcnix is a desktop app for managing Linux servers over SSH. The interface runs on your computer. Arcnix uses your SSH account to open shells and run server commands, and SFTP to read and write remote files. The server needs an SSH service. It does not need an Arcnix agent.

## What you can do

- **Files and code.** Transfer files between your computer and the server, view images and PDFs, edit remote text and preview Markdown. On Windows x64, stream remote MP4 and MKV files.
- **Terminal.** Open an interactive shell in a remote folder or use the terminal inside the editor.
- **Monitoring and Docker.** Check resource usage, services and logs. Inspect containers, ports and volumes on Docker hosts.
- **Tasks and backups.** Review scripts across hosts, check the results of server-owned tasks and restore files from restic snapshots.

Desktop downloads are available for Windows x64, macOS on Apple Silicon, and Linux x64 and ARM64.
For Intel Macs, download the `arcnix-macos-amd64` artifact from a successful [CI run](https://github.com/arcnix-org/arcnix/actions/workflows/ci.yml).

## Try the local desktop

Choose **Try the local desktop** above the server search field. Each visit lets you choose **Read-only mode** (recommended) or **Read-write mode**. The demo uses your current account on Windows, macOS or Linux and requires no SSH service. Read-only mode lets you browse real files, view code and play supported media. Choose read-write mode to create, rename and save files or run terminal commands on your computer.

On Windows, Terminal uses the built-in Windows PowerShell first and can use PowerShell 7 in its standard location or on PATH if needed. If neither is available, Files and Monitor still work. Monitor reads CPU, memory and disk usage directly from Windows.

In the local desktop, double-click a DOCX, XLSX or PPTX file in Files, or launch **Office**. The reader supports worksheet and slide navigation and zoom. In the read-write local desktop, DOCX files open for editing. You can change text and paragraph styles, apply bold and italic formatting, and undo or redo edits. **Save** shows the current path for confirmation. Choose **Save as DOCX** to write a separate copy. Files up to 32 MiB are supported. Layout fidelity depends on Syntra’s Office importer.

On an SSH desktop with SFTP, double-click a DOCX, XLSX or PPTX in Files to open a local working copy. Each server profile has a separate cache. For Word files, **Save to server** first saves the DOCX locally, then checks the remote file’s contents and metadata before replacing it. If the upload fails or the remote file changed, Office shows the cached file’s location. Choose **Pending uploads** in Office to recover the working copy after a restart, even if the remote file was deleted. Retry the upload, or choose **Open server version** to read the current remote file and keep the local copy. Excel and PowerPoint files open for reading.

Demo desktop preferences are stored in `~/.arcnix/self-demo-server/local-<os>/desktop.json`. A custom configuration keeps the demo directory beside the configuration file. Existing local desktop preferences are imported on first use.

## Previously saved WSL servers on Windows

For a saved WSL profile, select it in **Servers** and click **Connect**. The default connection uses the distribution directly. Arcnix opens an interactive Linux shell through `wsl.exe` and runs file operations as the distribution’s default Linux user. No SSH server, password or keys are required.

Browse directories, create files and folders, preview UTF-8 text, and upload or download individual files. Copies preserve existing destinations. You can also open the current directory in a terminal or Windows File Explorer.

To use the full SSH server desktop, edit the profile, select **SSH**, and enter the port and credentials. This mode requires OpenSSH Server on a port reachable from Windows over loopback. Arcnix verifies the distribution’s SSH host public keys before authentication.

## Feature demos

### SFTP file transfer

Upload a local file, pause and resume the transfer, then download a file from the server.

![Arcnix shows local and remote folders side by side with upload and download progress in the transfer queue.](docs/media/file-transfer.gif)

### Remote code editing

Edit a Markdown file on the server, save it and open the preview beside the source.

![Arcnix opens handover.md in the code editor and renders the saved changes in a split Markdown preview.](docs/media/remote-editing.gif)

### Server monitoring

Check network throughput, then open the Services tab to read Nginx logs.

![Arcnix Monitor switches from network interface statistics to the logs for nginx.service.](docs/media/server-monitoring.gif)

### Docker

Start a stopped container and inspect its logs, volume mounts and port mappings.

![Arcnix starts report-worker, reads its logs, then switches to the Volumes and Ports views.](docs/media/docker.gif)

[Development](docs/development.md) · [Changelog](CHANGELOG.md) · [Apache-2.0 license](LICENSE)
