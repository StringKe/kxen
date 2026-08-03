---
note-type: convention
description: Journal with run_id for resume
date: 2026-07-24
---

Pass `run_id` to the workflow tool to enable JSONL persistence in `data_dir/workflow-journals`. The journal is scoped by Session, `run_id`, and script hash; each completed `agent()` call is keyed by role and prompt. Matching calls can resume for 7 days, while a different Session, script, role, or prompt always misses the cache.
