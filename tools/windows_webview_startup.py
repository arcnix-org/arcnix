"""Launch the Windows GUI without a WebView2 folder override and verify its profile."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time


def launch(app):
    with tempfile.TemporaryDirectory(prefix="arcnix-startup-") as folder:
        config = Path(folder).resolve() / "config.json"
        config.write_text(json.dumps({"version": 1, "config": {
            "groups": [], "servers": [], "trusted_hosts": [],
            "settings": {"language": "English", "show_hidden_files": False,
                         "resource_interval_seconds": 3, "updates": {"automatic_checks": False}}}}),
            encoding="utf-8")
        marker = config.parent / "updates/startup-smoke.ok"
        marker.parent.mkdir()
        digest = hashlib.sha256(str(config).encode()).hexdigest()
        profile = Path(os.environ["LOCALAPPDATA"]) / "Arcnix/WebView2/configs" / digest
        assert not profile.exists(), profile
        env = os.environ.copy()
        env.pop("WEBVIEW2_USER_DATA_FOLDER", None)
        process = subprocess.Popen([app, "--config", config, "--update-health", marker],
                                   cwd=app.parent, env=env)
        try:
            deadline = time.monotonic() + 45
            while not marker.is_file() and time.monotonic() < deadline:
                if process.poll() is not None:
                    raise RuntimeError(f"GUI exited before startup acknowledgement: {process.returncode}")
                time.sleep(0.2)
            assert marker.read_text().startswith("arcnix "), marker
            assert (profile / "shell/EBWebView").is_dir(), profile
            assert not app.with_suffix(".exe.WebView2").exists(), app
            print(f"GUI started from {app.parent} with WebView2 in {profile / 'shell'}")
        finally:
            if process.poll() is None:
                process.terminate()
            process.wait(timeout=15)
            remove_test_profile(profile)


def remove_test_profile(profile):
    # WebView2 child processes can remove, create or briefly lock cache files during shutdown.
    deadline = time.monotonic() + 15
    while profile.exists():
        try:
            shutil.rmtree(profile)
        except OSError as error:
            if error.winerror not in (2, 3, 5, 32, 33, 145) or time.monotonic() >= deadline:
                raise
            time.sleep(0.25)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("exe", type=Path)
    parser.add_argument("--stage-under", type=Path, help="test a copy in a read-only installation directory")
    args = parser.parse_args()
    if args.stage_under is None:
        launch(args.exe.resolve())
        return
    with tempfile.TemporaryDirectory(dir=args.stage_under.resolve(), prefix="arcnix-install-") as folder:
        app = Path(folder) / "arcnix.exe"
        shutil.copyfile(args.exe, app)
        sid = "*" + subprocess.check_output([
            "powershell.exe", "-NoProfile", "-NonInteractive", "-Command",
            "[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value"
        ], text=True).strip()
        # Deny file/subdirectory creation even if the CI runner is an administrator.
        subprocess.run(["icacls", folder, "/deny", sid + ":(WD,AD)"], check=True)
        try:
            launch(app)
        finally:
            subprocess.run(["icacls", folder, "/remove:d", sid], check=True)


if __name__ == "__main__":
    main()
