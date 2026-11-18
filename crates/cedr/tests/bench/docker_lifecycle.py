"""Exercise real Docker resource lifetimes with local images and no vendor requests."""
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time


def main(binary: str, suite: str) -> None:
    docker = shutil.which("docker")
    assert docker
    image = os.environ.get("CEDR_BENCH_TEST_IMAGE", "ghcr.io/sheernox/cedr-bench-base:edition-1")
    image_id = subprocess.check_output(
        [docker, "image", "inspect", "--format", "{{.Id}}", image], text=True, timeout=10
    ).strip()

    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        shim = root / "docker"
        # Substitute a sleeping process only for vendor inference. Keep the task's
        # production Docker options and run the actual proxy and isolation checks.
        shim.write_text(f"#!{sys.executable}\n" + """import os
import sys
args = sys.argv[1:]
image = os.environ["TEST_IMAGE_ID"]
if args[0] in ("pull", "tag"):
    sys.exit(0)
if args[:2] == ["image", "inspect"]:
    print(image)
    sys.exit(0)
if args[0] == "run" and args[-2:] == ["codex", "--version"]:
    print("1.0.0")
    sys.exit(0)
if args[0] == "run" and image in args:
    position = args.index(image) + 1
    if args[position:position+2] == ["codex", "exec"]:
        args = ["run", "--cidfile", os.environ["TEST_READY"], *args[1:position], "sleep", "120"]
os.execv(os.environ["REAL_DOCKER"], ["docker", *args])
""")
        shim.chmod(0o700)
        processes = []
        resources = []

        def start(command: str, name: str) -> tuple[subprocess.Popen, str]:
            ready = root / name
            env = dict(PATH=f"{tmp}:{os.defpath}", HOME=tmp, TMPDIR=tmp,
                       XDG_STATE_HOME=str(root / "state"), CEDR_TELEMETRY="0",
                       CEDR_NO_AUTO_UPDATE="1", OPENAI_API_KEY="fixture",
                       REAL_DOCKER=docker, TEST_IMAGE_ID=image_id, TEST_READY=str(ready))
            child = subprocess.Popen(
                [binary, "bench", command, "--harness", "codex", "--model", "fixture",
                 "--agent-version", "1.0.0", "--suite", suite], env=env)
            processes.append(child)
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                if child.poll() is not None:
                    raise AssertionError(f"{name} exited before its test container started")
                if ready.exists() and (cid := ready.read_text().strip()):
                    run = subprocess.check_output(
                        [docker, "inspect", "-f", '{{index .Config.Labels "dev.cedr.bench.run"}}', cid],
                        timeout=5, text=True).strip()
                    assert run.startswith("cedr-bench-")
                    resources.append(run)
                    return child, run
                time.sleep(0.05)
            raise TimeoutError(f"{name} did not reach the Docker helper")

        def owned(run: str, kind: str) -> list[str]:
            args = ["ps", "-aq"] if kind == "container" else ["network", "ls", "-q"]
            return subprocess.check_output([docker, *args, "--filter",
                f"label=dev.cedr.bench.run={run}"], text=True, timeout=5).split()

        try:
            killed, dead = start("run", "first")
            survivor, live = start("probe", "second")
            killed.kill()
            killed.wait(timeout=5)
            assert len(owned(dead, "container")) == 2 and owned(dead, "network")
            newest, new = start("probe", "third")
            assert not owned(dead, "container") and not owned(dead, "network")
            assert len(owned(live, "container")) == 2 and owned(live, "network")
            # Model a delayed create that finishes after recovery unlinked the dead lease.
            subprocess.run([docker, "network", "create", "--internal", "--label",
                            f"dev.cedr.bench.run={dead}", dead], check=True, timeout=10)
            final, last = start("probe", "fourth")
            assert not owned(dead, "network")
            for child, run in [(survivor, live), (newest, new), (final, last)]:
                child.send_signal(signal.SIGINT)
                assert child.wait(timeout=25) > 0
                assert not owned(run, "container") and not owned(run, "network")
        finally:
            for child in processes:
                if child.poll() is None:
                    child.kill()
                    child.wait(timeout=5)
            for run in resources:
                for kind, args in [("container", ["rm", "-f"]), ("network", ["network", "rm"])]:
                    for resource in owned(run, kind):
                        subprocess.run([docker, *args, resource], timeout=10, check=True)


if __name__ == "__main__":
    main(*sys.argv[1:])
