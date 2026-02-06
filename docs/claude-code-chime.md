# Claude Code Notification Chime

Play a short chime when Claude Code finishes responding or asks for permission.

## Prerequisites

A `.wav` file at `C:\Users\caleb\.claude\sounds\chime.wav`. To convert an MP3 source:

```bash
ffmpeg -i source.mp3 -ar 44100 -ac 2 "C:\Users\caleb\.claude\sounds\chime.wav"
```

PowerShell's `System.Media.SoundPlayer` only supports `.wav`.

## Per-Project Setup

The hooks live in `.claude/settings.json` at the project root. This keeps the chime scoped to this project and out of the global config.

```jsonc
// .claude/settings.json
{
  "hooks": {
    "Stop": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "powershell -c \"(New-Object Media.SoundPlayer 'C:\\Users\\caleb\\.claude\\sounds\\chime.wav').PlaySync()\"",
            "timeout": 10
          }
        ]
      }
    ],
    "Notification": [
      {
        "matcher": "permission_prompt",
        "hooks": [
          {
            "type": "command",
            "command": "powershell -c \"(New-Object Media.SoundPlayer 'C:\\Users\\caleb\\.claude\\sounds\\chime.wav').PlaySync()\"",
            "timeout": 10
          }
        ]
      }
    ]
  }
}
```

### Hook Details

| Hook | Matcher | When it fires |
|------|---------|---------------|
| `Stop` | `""` (all) | Claude finishes a response |
| `Notification` | `"permission_prompt"` | Claude needs tool approval |

The `Notification` matcher is `"permission_prompt"` rather than empty to avoid firing on `idle_prompt` (60s inactivity), which would be annoying.

## Applying to Other Projects

Copy `.claude/settings.json` into any other project root, or move the hooks block into `~/.claude/settings.json` to make it global.

## Restart Required

Hooks are snapshotted at session start. Restart Claude Code after changing `.claude/settings.json`.
