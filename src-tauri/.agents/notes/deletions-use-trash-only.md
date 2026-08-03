---
note-type: convention
description: Deletions use trash only
date: 2026-07-24
---

User-requested removal of Workspace or knowledge content must go through trash for recoverability. Shell `rm` and equivalent destructive commands are denied; high-risk operations route through ApprovalBroker. Internal cleanup may unlink owned temporary, lock, cache, or transaction files after their exact path has been resolved.
