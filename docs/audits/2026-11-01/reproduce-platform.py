#!/usr/bin/env python3
"""Run isolated audit probes against this checkout's actual Rust libraries.

Uses temporary files, stub Docker commands, local HTTP, and Compose config only.
It does not start containers, change host configuration, or use saved credentials.
The DNS-origin probe resolves a public wildcard name to loopback without making
an HTTP request to it. Cargo build output goes to the existing target directory.
"""
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile


def main():
    here = Path(__file__).resolve().parent
    root = here.parents[2]
    with tempfile.TemporaryDirectory(prefix="vercelify-audit-") as work:
        work = Path(work)
        src = work / "src"
        src.mkdir()
        shutil.copyfile(here / "platform-probes.rs", src / "main.rs")
        project = (root / "agent/src/project.rs").read_text()
        start = project.index("pub fn private_origin(")
        end = project.index("/// Drop rewrites that proxy to a private origin", start)
        (src / "private_origin.rs").write_text("use std::net::IpAddr;\n" + project[start:end])
        manifest = [
            '[package]\nname = "vercelify-repo-audit"\nversion = "0.0.0"\nedition = "2021"\n',
            "[dependencies]\n",
        ]
        for package in ("vercelify-platform", "vercelify-core", "vercelify-mesh"):
            path = json.dumps(str(root / "crates" / package))
            manifest.append(f"{package} = {{ path = {path} }}\n")
        manifest.append('tokio = { version = "1", features = ["full"] }\nserde_json = "1"\ntempfile = "3"\n')
        (work / "Cargo.toml").write_text("".join(manifest))
        # Resolve from the repository's locked dependency versions. Cargo updates
        # only this temporary lockfile to add the standalone probe package.
        shutil.copyfile(root / "Cargo.lock", work / "Cargo.lock")
        env = os.environ.copy()
        env["CARGO_TARGET_DIR"] = str(root / "target")
        if not shutil.which("docker-compose"):
            docker = shutil.which("docker")
            if not docker:
                raise SystemExit("Docker Compose is required (configuration rendering only).")
            shim = work / "docker-compose"
            shim.write_text(f'#!/bin/sh\nexec {shlex.quote(docker)} compose "$@"\n')
            shim.chmod(0o700)
            env["PATH"] = str(work) + os.pathsep + env.get("PATH", "")
        subprocess.run(
            ["cargo", "run", "--offline", "--manifest-path", str(work / "Cargo.toml")],
            env=env,
            check=True,
        )


if __name__ == "__main__":
    main()
