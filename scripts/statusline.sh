#!/usr/bin/env bash
# Rosary statusline for Claude Code
# Colors match DESIGN.md: amber, sage, teal, bone

JSON=$(rsry status --json 2>/dev/null)
if [ $? -ne 0 ] || [ -z "$JSON" ]; then
    printf '○○○ rsry offline'
    exit 0
fi

OPEN=$(echo "$JSON" | jq -r '.open')
ACTIVE=$(echo "$JSON" | jq -r '.in_progress')
BLOCKED=$(echo "$JSON" | jq -r '.blocked')

OB="○"; [ "$OPEN" -gt 0 ] 2>/dev/null && OB="●"
AB="○"; [ "$ACTIVE" -gt 0 ] 2>/dev/null && AB="●"
BB="○"; [ "$BLOCKED" -gt 0 ] 2>/dev/null && BB="●"

printf '\e[38;2;224;148;82m%s\e[38;2;224;217;199m %s open \e[38;2;97;92;82m│\e[0m \e[38;2;115;184;115m%s\e[38;2;224;217;199m %s active \e[38;2;97;92;82m│\e[0m \e[38;2;102;173;166m%s\e[38;2;224;217;199m %s blocked\e[0m' "$OB" "$OPEN" "$AB" "$ACTIVE" "$BB" "$BLOCKED"
