---
note-type: convention
description: Dynamic workflows
date: 2026-07-24
---

Models emit QuickJS scripts run in a sandbox; orchestration uses `agent()`, `parallel()`, `phase()`, `log()`, and the read-only `CONSTRAINTS` snapshot. Ordinary JavaScript `await` expresses sequential dependencies.
