Check for agent plan conflicts.

Run: `~/.claude/hooks/plan.sh peers`

If any peer claims overlap paths you are currently working on (check your own
claim with `cat ~/github/agent-plans/plans/$(hostname -s).json`), say so in one
line and suggest whether to back off or proceed.

If there are no live peer claims and no overlap, reply with exactly: `plans clear`
Do not elaborate, do not summarize the plans, do not take any other action.
