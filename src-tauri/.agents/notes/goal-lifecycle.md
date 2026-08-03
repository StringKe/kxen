---
note-type: convention
description: Goal lifecycle
date: 2026-07-24
---

The reachable lifecycle is `draft -> active -> paused|complete|blocked|budget_limited|canceled`. `paused` and `blocked` may resume directly; `budget_limited` may return to `active` only through `adjust`, which raises or acknowledges the exhausted budget first. The `queued` enum value is retained only to deserialize older stores and can only be activated or canceled; no creation, RPC, UI, or agent-tool action enters it. A run freezes its Goal ID at start. Wall-clock budget counts only active time; compaction and completion verification consume tokens but not business turns. Usage from an already-started call is still charged after pause or cancel. A finite token budget with unavailable Provider usage fails closed to `budget_limited`. Enter `blocked` only after the same blocking reason repeats for 3 consecutive turns.
