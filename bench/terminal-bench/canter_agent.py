"""Canter's installed-agent adapter for Harbor 0.25.0, using local Docker.

References (no copied implementation): Harbor 0.25.0 BaseInstalledAgent,
Pi, DockerEnvironment and UnixOps' stdin-based tar upload.
"""

import asyncio
import base64
import io
import json
import os
import shlex
import tarfile
from pathlib import Path, PurePosixPath
from typing import Any

from harbor.agents.capabilities import AgentCapabilities
from harbor.agents.installed.base import BaseInstalledAgent, NonZeroAgentExitCodeError
from harbor.agents.options import InstalledAgentOptions
from harbor.constants import MAIN_SERVICE_NAME
from harbor.environments.docker.docker import DockerEnvironment
from harbor.models.agent.context import AgentContext, ModelUsage


class CanterOptions(InstalledAgentOptions):
    binary_path: str
    endpoint: str
    key_env: str
    model_settings: dict[str, Any]


class Canter(BaseInstalledAgent):
    capabilities = AgentCapabilities(reasoning_effort=True)
    options_model = CanterOptions
    options: CanterOptions

    @staticmethod
    def name() -> str:
        return "canter"

    @classmethod
    def preflight(cls, kwargs=None, env=None) -> None:
        options = cls.parse_options(kwargs, env)
        if not Path(options.binary_path).is_file():
            raise ValueError("binary_path must point to the musl canter executable")
        if not os.environ.get(options.key_env):
            raise ValueError(f"Set the host environment variable {options.key_env}")
        if options.key_env in (env or {}):
            raise ValueError("Pass only key_env, not the credential through --ae")
        if "context_window" not in options.model_settings:
            raise ValueError("model_settings must include the model's context_window")

    def get_version_command(self) -> str:
        return "/usr/local/bin/canter --version"

    async def install(self, environment) -> None:
        await self.ensure_system_dependencies(environment, ("ca_certificates",))
        await environment.upload_file(
            Path(self.options.binary_path), "/usr/local/bin/canter"
        )
        await self.exec_as_root(environment, "chmod 755 /usr/local/bin/canter")

    async def setup(self, environment) -> None:
        if not isinstance(environment, DockerEnvironment):
            raise ValueError("This adapter requires Harbor's local Docker environment")
        await super().setup(environment)
        # The released 0.25.0 has no configure hook; call ours after installation.
        await self.configure(environment)

    async def _exec_private(self, environment, command, *, stdin=None, timeout=None):
        # Do not use environment.exec: its streaming callback can persist output
        # before we redact it. The pinned Docker helper accepts an in-memory pipe
        # and, with check=False and no callback, never logs stdin or output.
        args = ["exec", "-T"]
        if environment.task_env_config.workdir:
            args += ["-w", environment.task_env_config.workdir]
        if environment.default_user is not None:
            args += ["-u", str(environment.default_user)]
        args += [MAIN_SERVICE_NAME, "sh", "-c", command]
        return await environment._run_docker_compose_command(
            args, check=False, stdin_data=stdin, timeout_sec=timeout
        )

    async def configure(self, environment) -> None:
        provider, separator, model = (self.model_name or "").partition("/")
        if not separator or not provider or not model:
            raise ValueError("Use --model provider/model")
        self._key = os.environ[self.options.key_env]
        settings = dict(self.options.model_settings)
        if self.options.reasoning_effort is not None:
            settings["default_reasoning_effort"] = self.options.reasoning_effort
        config = {
            "model": self.model_name,
            "providers": {
                provider: {
                    "base_url": self.options.endpoint,
                    "api_key": self._key,
                    "models": {model: settings},
                }
            },
        }
        result = await self._exec_private(
            environment,
            'set -eu; umask 077; mkdir -p "$HOME/.canter" /tmp/canter-harbor; '
            'cat > "$HOME/.canter/config.json"; '
            'chmod 600 "$HOME/.canter/config.json"',
            stdin=json.dumps(config).encode(),
        )
        if result.return_code:
            await self._remove_config(environment)
            raise RuntimeError("Could not create the container's canter configuration")

    async def _remove_config(self, environment) -> None:
        result = await self._exec_private(
            environment, 'rm -f "$HOME/.canter/config.json"', timeout=10
        )
        if result.return_code:
            raise RuntimeError("Could not remove the container's canter configuration")

    async def run(self, instruction, environment, context: AgentContext) -> None:
        self._errors = []
        error = None
        execution = asyncio.create_task(self._exec_private(
            environment,
            'echo $$ > /tmp/canter-harbor/exec.pid; '
            '[ ! -e /tmp/canter-harbor/stopping ] || exit 125; '
            "exec /usr/local/bin/canter exec --yolo -- "
            + shlex.quote(instruction)
            + " </dev/null > /tmp/canter-harbor/stdout.txt 2>&1",
        ))
        try:
            result = await asyncio.shield(execution)
        except (Exception, asyncio.CancelledError) as exc:
            error = exc
        # Harbor waits for cancellation to finish before grading. Shield the
        # cleanup even from a second cancellation; preserve CancelledError.
        cleanup = asyncio.create_task(self._cleanup(environment))
        while True:
            try:
                await asyncio.shield(cleanup)
                break
            except asyncio.CancelledError as exc:
                error = exc
                if cleanup.cancelled():
                    break
            except Exception as exc:
                if not isinstance(error, asyncio.CancelledError):
                    raise
                self._record_error("cleanup", exc)
                break
        # Reap the Docker exec client after its in-container process has stopped.
        await asyncio.gather(execution, return_exceptions=True)
        if error is not None:
            raise error
        if result.return_code:
            raise NonZeroAgentExitCodeError(
                f"canter exec exited {result.return_code}; see agent/stdout.txt"
            )

    def _record_error(self, phase, error):
        # Exception messages may include command output: store types only.
        self._errors.append({"phase": phase, "type": type(error).__name__})

    async def _stop_processes(self, environment):
        result = await self._exec_private(environment, r'''
set -eu
touch /tmp/canter-harbor/stopping
stop_pid() {
    pid=$1
    case "$pid" in ''|*[!0-9]*) return 1;; esac
    [ "$pid" -gt 1 ] || return 1
    # A zombie has no executable and cannot mutate the task filesystem.
    [ -e "/proc/$pid/exe" ] || return 0
    kill -TERM "$pid" 2>/dev/null || true
    count=0
    while [ -e "/proc/$pid/exe" ] && [ "$count" -lt 30 ]; do
        sleep 1
        count=$((count + 1))
    done
    if [ -e "/proc/$pid/exe" ]; then
        kill -KILL "$pid" 2>/dev/null || true
        count=0
        while [ -e "/proc/$pid/exe" ] && [ "$count" -lt 5 ]; do
            sleep 1
            count=$((count + 1))
        done
    fi
    [ ! -e "/proc/$pid/exe" ]
}
for file in /tmp/canter-harbor/exec.pid /tmp/canter-harbor/stats.pid; do
    if [ -f "$file" ]; then stop_pid "$(cat "$file")"; rm -f "$file"; fi
done
if [ -f "$HOME/.canter/server.json" ]; then
    pid=$(sed -n 's/.*"pid":[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$HOME/.canter/server.json")
    stop_pid "$pid"
fi
''', timeout=120)
        if result.return_code:
            raise RuntimeError("Could not stop canter before verification")

    async def _cleanup(self, environment):
        try:
            # Stop the daemon even on normal completion: subagents may be busy.
            await self._stop_processes(environment)
            try:
                await self._collect(environment)
            except Exception as exc:
                self._record_error("collection", exc)
        finally:
            try:
                await self._remove_config(environment)
            finally:
                # stats starts a fresh daemon for its read-only request.
                await self._stop_processes(environment)

    async def _collect(self, environment) -> None:
        result = await self._exec_private(
            environment,
            "sh -c 'echo $$ > /tmp/canter-harbor/stats.pid; "
            "exec /usr/local/bin/canter stats --last --json' "
            "> /tmp/canter-harbor/stats.json 2> /tmp/canter-harbor/stats.stderr; "
            "status=$?; printf '%s' \"$status\" > /tmp/canter-harbor/stats.exit; "
            'if [ -d "$HOME/.canter/sessions" ]; then '
            'cp -R "$HOME/.canter/sessions" /tmp/canter-harbor/sessions || exit 1; fi; '
            "tar -C /tmp/canter-harbor -cf /tmp/canter-harbor.tar . "
            "2>/tmp/canter-harbor-tar.stderr && "
            "printf 'CANTER_ARCHIVE_BEGIN\\n' && "
            "base64 /tmp/canter-harbor.tar && "
            "printf '\\nCANTER_ARCHIVE_END\\n'",
            timeout=60,
        )
        if result.return_code:
            raise RuntimeError("Could not collect canter stats and sessions")
        # Download through memory, then redact before any host write. Never
        # archive config.json, server state, or the whole ~/.canter directory.
        # Harbor's Docker helper merges stderr into stdout. Frame the payload
        # so Compose warnings cannot become archive bytes; tar stderr is separate.
        encoded = (result.stdout or "").split("CANTER_ARCHIVE_BEGIN\n", 1)[1]
        encoded, _ = encoded.split("\nCANTER_ARCHIVE_END\n", 1)
        archive = base64.b64decode("".join(encoded.split()), validate=True)
        redactions = 0
        with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
            for member in tar:
                if not member.isfile():
                    continue
                name = PurePosixPath(member.name)
                if name.is_absolute() or ".." in name.parts:
                    raise RuntimeError("Unexpected path in canter log archive")
                content = tar.extractfile(member).read()
                redactions += content.count(self._key.encode())
                content = content.replace(self._key.encode(), b"[REDACTED]")
                path = self.logs_dir / str(name).replace(self._key, "[REDACTED]")
                if not path.resolve().is_relative_to(self.logs_dir.resolve()):
                    raise RuntimeError("Canter log path escapes the trial log directory")
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(content)
        (self.logs_dir / "collection.json").write_text(
            json.dumps({"credential_redactions": redactions}) + "\n"
        )
        if (self.logs_dir / "stats.exit").read_text() != "0":
            raise RuntimeError("canter stats failed; see agent/stats.stderr")

    def populate_context_post_run(self, context: AgentContext) -> None:
        self._errors = getattr(self, "_errors", [])
        context.n_input_tokens = context.n_cache_tokens = context.n_output_tokens = None
        context.cost_usd = context.model_usage = None
        context.metadata = {
            "reasoning_effort": self.options.reasoning_effort,
            "errors": self._errors,
        }
        try:
            context.metadata["credential_redactions"] = json.loads(
                (self.logs_dir / "collection.json").read_text()
            )["credential_redactions"]
            if any(item["phase"] == "collection" for item in self._errors):
                return
            report = json.loads((self.logs_dir / "stats.json").read_text())
            overview = report["overview"]
            children = report.get("subAgents", [])
            context.metadata.update({
                "canter_session_id": report["sessionId"],
                "sub_agent_count": len(children),
                "sub_agent_empty_usage_count": sum(child["usage"] is None for child in children),
            })
            aggregate = overview.get("usageWithSubAgents")
            if aggregate is not None:
                usages = [aggregate]
            else:
                parent = overview["usage"]
                usages = ([parent["total"]] if parent is not None else [])
                usages += [child["usage"] for child in children if child["usage"] is not None]
            cached = sum(item["cachedInputTokens"] for item in usages)
            uncached = sum(item["uncachedInputTokens"] for item in usages)
            output = sum(item["outputTokens"] for item in usages)
            cost = (None if not usages or any(item["cost"]["unknown"] for item in usages)
                    else sum(item["cost"]["usd"] for item in usages))
            model_usage = ModelUsage(n_input_tokens=cached + uncached,
                                     n_cache_tokens=cached, n_output_tokens=output, cost_usd=cost)
            context.n_input_tokens = model_usage.n_input_tokens
            context.n_cache_tokens = model_usage.n_cache_tokens
            context.n_output_tokens = model_usage.n_output_tokens
            context.cost_usd = model_usage.cost_usd
            context.model_usage = {self.model_name: model_usage}
        except Exception as exc:
            self._record_error("usage", exc)
