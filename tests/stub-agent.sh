#!/bin/sh
# Stand-in for `claude -p ... --output-format json`. Makes a predictable commit and replies to
# every event id found in the prompt. Pass `--leak` (via agent_args) to add a fake token.
set -e
# The prompt arrives on stdin (like `claude -p` reading standard input).
prompt="$(cat)"
echo "stub change" >> stub.txt
case " $* " in *" --leak "*) leak=1;; esac
if [ -n "$leak" ]; then echo 'token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789AB"' >> stub.txt; fi
git add stub.txt
git commit -q -m "stub: address review"
sha=$(git rev-parse --short HEAD)
replies=""
for id in $(printf '%s' "$prompt" | sed -n 's/.*event `\([^`]*\)`.*/\1/p'); do
  replies="$replies{\\\"event\\\":\\\"$id\\\",\\\"body\\\":\\\"Done in $sha\\\"},"
done
replies="${replies%,}"
# Review steps also give a verdict.
verdict=""
case "$prompt" in *"Task: review"*) verdict=',\"verdict\":\"approve\"';; esac
printf '{"result":"ok\\n```nightporter\\n{\\"summary\\":\\"stub fixed it\\",\\"replies\\":[%s]%s}\\n```","total_cost_usd":0.01}\n' "$replies" "$verdict"
