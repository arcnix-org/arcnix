//! Native shell checks in an isolated Linux home, with a fake Docker executable.
#[cfg(any(windows, target_os = "linux"))]
#[test]
#[ignore = "requires Linux (or WSL Ubuntu) and Python 3; uses no Docker daemon"]
fn remote_preferences_are_atomic_private_and_docker_uses_bounded_local_commands()
-> anyhow::Result<()> {
    use std::io::Write;
    use std::process::{Command, Output, Stdio};

    use anyhow::{Result, ensure};
    use arcnix_core::Theme;
    use arcnix_core::browser::{BOOKMARK_LIMIT, Bookmark, URL_LIMIT};
    use arcnix_core::desktop_theme::{Preset, Selection};
    use arcnix_core::docker::{self, Operation};
    use arcnix_core::remote_tasks::quote as q;
    use arcnix_core::server_apps::{DesktopApp, DesktopPreferences, DesktopWindow, FolderColor};
    use arcnix_core::sync::{Document, Snapshot, decode_desktop, desktop_commit_command};

    #[cfg(windows)]
    let distro = std::env::var("ARCNIX_TEST_SHELL_DISTRIBUTION")?;
    let raw = |script: &str| -> Result<Output> {
        #[cfg(windows)]
        let mut shell = Command::new("wsl.exe");
        #[cfg(windows)]
        shell.args(["-d", &distro, "--exec", "/bin/sh"]);
        #[cfg(target_os = "linux")]
        let mut shell = Command::new("/bin/sh");
        let mut child = shell
            .arg("-s")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(format!("set -eu\n{script}").as_bytes())?;
        Ok(child.wait_with_output()?)
    };
    let run = |script: &str| -> Result<String> {
        let output = raw(script)?;
        ensure!(
            output.status.success(),
            "Native shell check failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8(output.stdout)?)
    };
    let root = format!("/tmp/arcnix-server-apps-{}", arcnix_core::new_id());
    ensure!(
        root.starts_with("/tmp/arcnix-server-apps-") && !root.contains(' '),
        "Invalid test root"
    );
    let home = format!("{root}/login home 中文");
    let desktop_shell =
        std::env::var("ARCNIX_TEST_DESKTOP_LOGIN_SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let invoke = |command: &str| -> String {
        let shell = if command.starts_with("# arcnix:desktop:domains:") {
            &desktop_shell
        } else {
            "/bin/sh"
        };
        format!(
            "env HOME={} XDG_RUNTIME_DIR={} PATH={} DOCKER_HOST=tcp://wrong.invalid:2375 DOCKER_CONTEXT=wrong ARCNIX_NATIVE_FIXTURE={} {} -c {}",
            q(&home),
            q(&format!("{root}/runtime")),
            q(&format!("{root}/bin:/usr/bin:/bin")),
            q(&root),
            q(shell),
            q(command)
        )
    };
    run(&format!(
        "umask 077\nmkdir -p -- {} {}\nprintf '%s' {} > {}/bin/docker\nchmod 700 {}/bin/docker",
        q(&home),
        q(&format!("{root}/bin")),
        q(include_str!("server_apps_native_fixture.py")),
        q(&root),
        q(&root)
    ))?;
    let result = (|| -> Result<()> {
        let document = Document::Desktop;
        let read = document.read_command()?;
        let commit = |base: &Snapshot, prefs: &DesktopPreferences| -> Result<String> {
            let (command, payload) = desktop_commit_command(base, prefs)?;
            Ok(format!(
                "printf '%s' {} | {}",
                q(std::str::from_utf8(&payload)?),
                invoke(&command)
            ))
        };
        ensure!(
            run(&invoke(&read))? == "M\n",
            "Missing preferences did not load defaults"
        );
        ensure!(
            run(&format!(
                "test ! -e {} && printf absent",
                q(&format!("{home}/.arcnix"))
            ))? == "absent",
            "Read created server files"
        );
        let prefs = DesktopPreferences {
            version: 1,
            theme: Theme::Dark,
            browser_bookmarks: (0..BOOKMARK_LIMIT)
                .map(|i| {
                    let prefix = format!("https://example.com/{i}?q=");
                    Bookmark {
                        title: format!("Bookmark {i}"),
                        url: format!("{prefix}{}", "x".repeat(URL_LIMIT - prefix.len())),
                    }
                })
                .collect(),
            windows: [
                (
                    DesktopApp::Files,
                    DesktopWindow {
                        maximized: true,
                        ..Default::default()
                    },
                ),
                (DesktopApp::Code, DesktopWindow::default()),
            ]
            .into(),
            ..Default::default()
        };
        ensure!(
            run(&commit(&Snapshot(None), &prefs)?)? == "OK\n",
            "Initial save failed"
        );
        let output = run(&invoke(&read))?;
        let data = output
            .strip_prefix("F\n")
            .expect("present desktop document");
        ensure!(
            data.len() > 128 * 1024,
            "Native shell fixture did not exercise streamed input"
        );
        let base = Snapshot(Some(data.as_bytes().to_vec()));
        let loaded = decode_desktop(&base)?;
        ensure!(loaded == prefs, "Remote preference round trip failed");
        let config = format!("{home}/.arcnix/desktop");
        let files: serde_json::Value = serde_json::from_str(&run(&format!(
            "cat {}",
            q(&format!("{config}/apps/files.json"))
        ))?)?;
        let code: serde_json::Value = serde_json::from_str(&run(&format!(
            "cat {}",
            q(&format!("{config}/apps/code.json"))
        ))?)?;
        ensure!(
            files["window"]["maximized"] == true && code["window"]["maximized"] == false,
            "Window state lost application ownership"
        );
        ensure!(
            files.get("settings").is_some() && code.get("settings").is_none(),
            "File settings leaked to another application"
        );
        let system = format!("{config}/system.json");
        let modes = run(&format!(
            "stat -c '%a' {} {}",
            q(&format!("{home}/.arcnix")),
            q(&system)
        ))?;
        ensure!(
            modes == "700\n600\n",
            "Preferences were not private: {modes}"
        );
        let inode = run(&format!("stat -c '%i' {}", q(&system)))?;
        let mut next = DesktopPreferences {
            theme: Theme::Light,
            ..prefs.clone()
        };
        next.files.colors.insert("/work".into(), FolderColor::Blue);
        next.windows.get_mut(&DesktopApp::Code).unwrap().maximized = true;
        let (interrupted, _) = desktop_commit_command(&base, &next)?;
        ensure!(
            interrupted.starts_with("# arcnix:desktop:domains:commit\n"),
            "Multi-file save did not use snapshot publication"
        );
        ensure!(
            !raw(&format!("printf incomplete | {}", invoke(&interrupted)))?
                .status
                .success(),
            "An incomplete upload was published"
        );
        ensure!(
            run(&invoke(&read))? == output,
            "An incomplete upload changed the active snapshot"
        );
        ensure!(
            run(&commit(&base, &next)?)? == "OK\n",
            "Updated save failed"
        );
        let updated = run(&invoke(&read))?;
        let updated = updated
            .strip_prefix("F\n")
            .expect("present desktop document");
        ensure!(
            decode_desktop(&Snapshot(Some(updated.as_bytes().to_vec())))? == next,
            "Publish exposed a mixture of revisions across application files"
        );
        ensure!(
            run(&commit(&base, &prefs)?)? == "STALE\n",
            "Stale revision overwrote the latest settings"
        );
        ensure!(
            inode != run(&format!("stat -c '%i' {}", q(&system)))?,
            "Save overwrote the original inode"
        );

        let active_generation = || run(&format!("readlink -- {}", q(&config)));
        let generation = active_generation()?;
        let files_path = format!("{config}/apps/files.json");
        let unrelated = format!("{config}/apps/web_browser.json");
        run(&format!("printf '\\n' >> {}", q(&unrelated)))?;
        let file_identity = |path: &str| run(&format!("stat -c '%i %y' -- {}", q(path)));
        let unchanged_browser = file_identity(&unrelated)?;
        let files_before = file_identity(&files_path)?;
        let output = run(&invoke(&read))?;
        let current = Snapshot(Some(
            output.strip_prefix("F\n").unwrap().as_bytes().to_vec(),
        ));
        let mut files_changed = next.clone();
        files_changed
            .files
            .colors
            .insert("/other".into(), FolderColor::Blue);
        let (single, payload) = desktop_commit_command(&current, &files_changed)?;
        ensure!(
            single.starts_with("# arcnix:desktop:domains:file\n") && payload.len() < 4096,
            "Files-only save uploaded a full snapshot"
        );
        ensure!(
            !raw(&format!("printf incomplete | {}", invoke(&single)))?
                .status
                .success()
                && file_identity(&files_path)? == files_before,
            "Incomplete single-file upload changed Files"
        );
        let mut other_change = next.clone();
        other_change.theme = Theme::System;
        ensure!(run(&commit(&current, &other_change)?)? == "OK\n");
        ensure!(
            run(&commit(&current, &files_changed)?)? == "STALE\n"
                && file_identity(&files_path)? == files_before,
            "A stale single-file save was published"
        );
        let output = run(&invoke(&read))?;
        let latest = Snapshot(Some(
            output.strip_prefix("F\n").unwrap().as_bytes().to_vec(),
        ));
        files_changed.theme = other_change.theme;
        let save_files = commit(&latest, &files_changed)?;
        ensure!(run(&save_files)? == "OK\n");
        let files_after = file_identity(&files_path)?;
        ensure!(
            files_after != files_before
                && run(&save_files)? == "OK\n"
                && file_identity(&files_path)? == files_after,
            "Single-file save did not replace only once"
        );
        let output = run(&invoke(&read))?;
        let saved = Snapshot(Some(
            output.strip_prefix("F\n").unwrap().as_bytes().to_vec(),
        ));
        ensure!(
            active_generation()? == generation
                && file_identity(&unrelated)? == unchanged_browser
                && decode_desktop(&saved)? == files_changed,
            "Single-file saves changed the generation, touched the browser, or lost an edit"
        );
        let leftovers = run(&format!(
            "find {} -name '.stage.*' -o -name '.file.*' -o -name '.desktop.next.*'",
            q(&format!("{home}/.arcnix"))
        ))?;
        ensure!(
            leftovers.is_empty(),
            "Staging files left behind: {leftovers}"
        );

        let state = format!("{home}/.arcnix");
        let previous_link = format!("{state}/desktop.saved");
        let sparse = "desktop-revisions/generation.Legacy01";
        let sparse_system = r#"{"theme":"Dark"}"#;
        run(&format!(
            "umask 077\nmkdir -- {0}/{1}\nprintf '%s' {2} > {0}/{1}/system.json\nmv -- {3} {4}\nln -s -- {1} {3}",
            q(&state),
            q(sparse),
            q(sparse_system),
            q(&config),
            q(&previous_link),
        ))?;
        let read_snapshot = || -> Result<Snapshot> {
            let output = run(&invoke(&read))?;
            Ok(Snapshot(Some(
                output.strip_prefix("F\n").unwrap().as_bytes().to_vec(),
            )))
        };
        let before = read_snapshot()?;
        let mut edited = decode_desktop(&before)?;
        ensure!(
            edited
                == DesktopPreferences {
                    theme: Theme::Dark,
                    ..Default::default()
                },
            "A sparse generation did not keep its existing values and supply defaults"
        );
        run(&format!(
            "test ! -e {0}/apps && test ! -e {0}/preferences",
            q(&config)
        ))?;
        edited.appearance.themes.selection = Selection::Preset(Preset::Aurora);
        let save_theme = commit(&before, &edited)?;
        ensure!(run(&save_theme)? == "OK\n" && run(&save_theme)? == "OK\n");
        ensure!(
            decode_desktop(&read_snapshot()?)? == edited
                && active_generation()? == format!("{sparse}\n")
                && run(&format!("cat {}", q(&system)))? == sparse_system,
            "Saving a theme did not preserve the sparse generation and its existing file"
        );
        run(&format!(
            "test ! -e {0}/apps && test -f {0}/preferences/desktop.json",
            q(&config)
        ))?;
        run(&format!("mv -fT -- {} {}", q(&previous_link), q(&config)))?;

        run(&format!(
            "mv {} {}\nprintf keep > {}\nln -s {} {}",
            q(&system),
            q(&format!("{config}/system.saved")),
            q(&format!("{root}/outside")),
            q(&format!("{root}/outside")),
            q(&system)
        ))?;
        ensure!(
            !raw(&commit(&base, &prefs)?)?.status.success(),
            "Linked preference file was followed"
        );
        ensure!(
            run(&format!("cat {}", q(&format!("{root}/outside"))))? == "keep",
            "Linked target changed"
        );
        run(&format!(
            "rm -- {}\nmv {} {}\nln -s {} {}",
            q(&system),
            q(&format!("{home}/.arcnix")),
            q(&format!("{home}/original-dir")),
            q(&format!("{home}/original-dir")),
            q(&format!("{home}/.arcnix"))
        ))?;
        ensure!(
            !raw(&invoke(&read))?.status.success(),
            "Linked preference directory was followed"
        );
        ensure!(
            !raw(&commit(&base, &prefs)?)?.status.success(),
            "Linked preference directory was written"
        );

        let text = run(&invoke(&docker::snapshot_command()))?;
        let snapshot = docker::parse_snapshot(&text)?;
        ensure!(
            snapshot.containers.len() == 1 && snapshot.volumes.len() == 1,
            "Native shell inventory parsing failed"
        );
        let id = "a".repeat(64);
        let details: docker::ContainerDetails =
            serde_json::from_str(&run(&invoke(&docker::inspect_command(&id)?))?)?;
        ensure!(
            details.id == id && details.bindings["80/tcp"][0]["HostPort"] == "8080",
            "Inspect lost bound ports"
        );
        let volume: docker::VolumeDetails =
            serde_json::from_str(&run(&invoke(&docker::volume_command("web-data")?))?)?;
        ensure!(
            volume.mountpoint.ends_with("/_data"),
            "Volume inspect lost mountpoint"
        );
        let logs = raw(&invoke(&docker::logs_command(&id)?))?;
        ensure!(
            logs.status.success()
                && logs.stdout == b"container output\ncontainer error\ncontainer output again\n"
                && logs.stderr.is_empty(),
            "Container streams were not merged in order"
        );
        for operation in [
            Operation::Start,
            Operation::Stop,
            Operation::Restart,
            Operation::Configure {
                restart: "on-failure:3".into(),
                cpu_millis: Some(1500),
                memory_mib: Some(512),
            },
        ] {
            run(&invoke(&docker::operation_command(&id, &operation)?))?;
        }
        let trace = run(&format!("cat {}", q(&format!("{root}/docker-argv.jsonl"))))?;
        for line in trace.lines() {
            let args: Vec<String> = serde_json::from_str(line)?;
            ensure!(
                args.starts_with(&["--host".into(), "unix:///var/run/docker.sock".into()]),
                "Docker used a remote context"
            );
            ensure!(
                !args
                    .iter()
                    .any(|a| a == "--follow" || a.contains("Config.Env")),
                "Unbounded or sensitive Docker query"
            );
        }
        ensure!(
            trace.contains("on-failure:3") && trace.contains("1.500") && trace.contains("512m"),
            "Configuration arguments changed"
        );
        Ok(())
    })();
    let cleanup = run(&format!("rm -rf -- {}", q(&root)));
    result?;
    cleanup?;
    Ok(())
}
