# 阿牛桌面（Arcnix）

[English](README.md) · [下载](https://arcnix.org/zh-cn/#downloads) · [快速上手](https://arcnix.org/zh-cn/docs/quick-start/) · [使用文档](https://arcnix.org/zh-cn/docs/)

阿牛桌面是一个通过 SSH 管理 Linux 服务器的桌面应用。界面运行在你的电脑上，终端和管理命令使用你的 SSH 账号在服务器上执行，远程文件通过 SFTP 读写。服务器开启 SSH 即可连接，无需安装阿牛桌面服务端。

## 主要功能

- **文件与代码**　在本机和服务器之间传输文件，查看图片、阅读 PDF、编辑远程文本和预览 Markdown。Windows x64 还支持播放远程 MP4 和 MKV。
- **终端**　在指定远程目录打开交互式终端，也可以在代码编辑器里使用终端。
- **监控与 Docker**　查看资源用量、服务和日志。在运行 Docker 的服务器上检查容器、端口和卷。
- **任务与备份**　在多台服务器上审核并运行脚本，查询已提交任务的结果，从 restic 快照恢复文件。

客户端提供 Windows x64、macOS Apple Silicon，以及 Linux x64 和 ARM64 下载包。
Intel Mac 用户可从通过的 [CI 运行记录](https://github.com/arcnix-org/arcnix/actions/workflows/ci.yml)下载 `arcnix-macos-amd64` 测试包。

## 体验本机桌面

点击服务器搜索框上方的「体验本机桌面」。每次进入时，可选择「只读模式」（推荐）或「读写模式」。演示使用当前 Windows、macOS 或 Linux 账号，无需配置 SSH。只读模式可以浏览真实文件、查看代码和播放支持的媒体。选择读写模式后，可以新建、重命名和保存文件，也可以在终端中运行本机命令。

Windows 终端优先使用系统自带的 Windows PowerShell，也支持安装在标准位置或加入 PATH 的 PowerShell 7。如果两者都找不到，文件管理和监控仍可使用。监控直接从 Windows 获取 CPU、内存和磁盘使用情况。

在本机桌面的「文件」中双击 DOCX、XLSX 或 PPTX，或从启动器打开「Office」。支持缩放、切换工作表和幻灯片。在本机「读写模式」中，DOCX 默认进入编辑器，可修改正文、设置段落样式、加粗或斜体，也可以撤销和重做。「保存」会显示当前文件路径，确认后写回。也可选择「另存 DOCX」保存副本。单个文件上限为 32 MiB，复杂版式按 Syntra 的导入能力显示。

在支持 SFTP 的 SSH 桌面中，双击远端 DOCX、XLSX 或 PPTX 即可打开本地缓存副本。每个服务器配置使用独立的缓存目录。编辑 Word 后选择「保存到服务器」，阿牛桌面会先保存本地副本，再核对远端内容和文件属性，确认版本一致才上传替换。遇到冲突或上传失败时，Office 会显示缓存路径。重启后可从 Office 的「待上传文件」找回副本并重试，即使远端原件已经删除。也可以选择「打开服务器版本」，同时保留本地副本。Excel 和 PowerPoint 文件支持阅读。

演示桌面偏好保存在 `~/.arcnix/self-demo-server/local-<os>/desktop.json`。使用自定义配置时，演示目录位于配置文件旁。首次进入时会导入原有的本机桌面偏好。

## 使用已保存的 WSL 服务器（Windows）

在服务器侧栏选中已保存的 WSL 配置，点击「连接」即可默认使用本机直连。终端通过 `wsl.exe` 启动交互式 Linux shell，文件操作使用发行版的默认 Linux 账号。直连无需安装 SSH 服务或配置密码、密钥。

在「文件」中浏览目录、新建文件和文件夹、预览 UTF-8 文本，或在 Windows 与 WSL 之间上传和下载单个文件。同名目标会保留原文件。也可以从当前目录打开终端或 Windows 文件资源管理器。

若需要通过 SSH 使用完整服务器桌面，在编辑配置时选择 **SSH**，填写可从 Windows 回环地址访问的端口和认证信息。该模式需要发行版运行 OpenSSH Server，阿牛桌面会在认证前核对发行版的主机公钥。

## 功能演示

### SFTP 文件传输

上传本地文件，在传输队列中暂停和恢复上传，再从服务器下载文件。

![阿牛桌面并排显示本地和远程目录，传输队列显示上传与下载进度。](docs/media/file-transfer.gif)

### 远程代码编辑

编辑并保存服务器上的 Markdown 文件，在源码右侧查看渲染效果。

![阿牛桌面在代码编辑器中打开 handover.md，保存修改并在分栏中显示 Markdown 预览。](docs/media/remote-editing.gif)

### 服务器监控

查看网络吞吐量，再切换到服务页面读取 Nginx 日志。

![阿牛桌面的监控窗口从网络接口统计切换到 nginx.service 的日志。](docs/media/server-monitoring.gif)

### Docker

启动已停止的容器，查看容器日志、卷挂载和端口映射。

![阿牛桌面启动 report-worker 容器并查看它的日志，然后切换到卷和端口页面。](docs/media/docker.gif)

[开发说明](docs/development.md) · [更新记录](CHANGELOG.md) · [Apache-2.0 许可证](LICENSE)
