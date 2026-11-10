"""No-model Harbor timeout probe; never use this agent for benchmark scores."""

import json
from datetime import datetime, timezone

from canter_agent import Canter


class TimeoutProbe(Canter):
    async def setup(self, environment):
        if self.options.endpoint != "https://192.0.2.1:443/v1":
            raise ValueError("The probe requires the nonresponding TEST-NET endpoint")
        self.observations = []
        await super().setup(environment)

    async def snapshot(self, environment, phase):
        result = await self._exec_private(environment, r'''
set -eu
printf 'CANTER_PROCESS_BEGIN\n'
for entry in /proc/[0-9]*/exe; do
    target=$(readlink "$entry" 2>/dev/null) || continue
    if [ "$target" = /usr/local/bin/canter ]; then
        pid=${entry#/proc/}
        printf '%s\n' "${pid%/exe}"
    fi
done
printf 'CANTER_PROCESS_END\n'
if [ -e "$HOME/.canter/config.json" ]; then printf 'CONFIG_PRESENT\n'; fi
if [ -f /tmp/canter-harbor/exec.pid ]; then
    printf 'EXEC_PID='; cat /tmp/canter-harbor/exec.pid
fi
''', timeout=10)
        if result.return_code:
            raise RuntimeError("Timeout probe could not inspect processes")
        text = result.stdout or ""
        pids = text.split("CANTER_PROCESS_BEGIN\n", 1)[1].split("CANTER_PROCESS_END\n", 1)[0]
        observation = {
            "phase": phase, "checked_at": datetime.now(timezone.utc).isoformat(),
            "canter_pids": [int(pid) for pid in pids.split()],
            "config_present": "CONFIG_PRESENT" in text,
            "exec_pid": next((int(line.partition("=")[2]) for line in text.splitlines()
                              if line.startswith("EXEC_PID=")), None),
        }
        self.observations.append(observation)
        return observation

    async def _stop_processes(self, environment, *, stop_server=True):
        before = await self.snapshot(environment, "before_stop")
        await super()._stop_processes(environment, stop_server=stop_server)
        observation = await self.snapshot(environment, "after_stop")
        observation["stop_server"] = stop_server
        if ((stop_server and observation["canter_pids"])
                or before["exec_pid"] in observation["canter_pids"]):
            raise RuntimeError("Canter survived PID cleanup")

    async def run(self, instruction, environment, context):
        try:
            await super().run(instruction, environment, context)
        finally:
            observation = await self.snapshot(environment, "before_return_to_harbor")
            (self.logs_dir / "timeout-probe.json").write_text(json.dumps({
                "adapter_test_only": True, "observations": self.observations,
            }, indent=2) + "\n")
            if observation["config_present"]:
                raise RuntimeError("Timeout probe found incomplete cleanup")
