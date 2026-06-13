#requires -Version 5
<#
.SYNOPSIS
  DOGKBD Claude Code status hook — reports a Claude Code instance's busy/idle
  state to the router so the multiplexer knows whether the destination can
  accept input.

.DESCRIPTION
  Install in a project's .claude/settings.json so it fires on UserPromptSubmit
  (busy) and Stop (idle). Tag each project with a distinct -Instance so the
  router tracks them separately. The tea-leaves game may omit -Instance (the
  router defaults to "game"); the trophy factory should pass -Instance trophy.

.EXAMPLE
  .claude/settings.json hooks entry (trophy_factory):
    {
      "hooks": {
        "UserPromptSubmit": [{ "hooks": [{ "type": "command",
          "command": "powershell -NoProfile -ExecutionPolicy Bypass -File C:/Projects/DogKeyboard/tools/claude-status.ps1 -Status busy -Instance trophy" }]}],
        "Stop": [{ "hooks": [{ "type": "command",
          "command": "powershell -NoProfile -ExecutionPolicy Bypass -File C:/Projects/DogKeyboard/tools/claude-status.ps1 -Status idle -Instance trophy" }]}]
      }
    }
#>
param(
    [Parameter(Mandatory)][ValidateSet("busy", "idle")][string]$Status,
    [string]$Instance = "",
    [string]$Router = "http://localhost:8080"
)

$uri = "$Router/claude-status"
if ($Instance -ne "") { $uri += "?instance=$Instance" }

try {
    Invoke-RestMethod -Uri $uri -Method Post `
        -ContentType "application/json" `
        -Body "{`"status`":`"$Status`"}" -TimeoutSec 2 | Out-Null
} catch {
    # Best-effort: never block Claude Code if the router is down.
}
