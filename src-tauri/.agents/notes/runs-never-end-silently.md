---
note-type: convention
description: Runs never end silently
date: 2026-07-24
---

Every agent run path must persist and publish a completed, interrupted, or error terminal. Durable PendingQueue delivery, atomic run-token handoff, fallback handling, and explicit completion paths enforce this; UI stream silence is never treated as a terminal.
