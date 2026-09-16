---
id: q-ca38
title: 'one mass-edit fix gate: [history].max_cochange_commit_size caps both co-change evidence and fix counting; fix_mass_edit_cap (default on) is the knob for the fix half and main''s mass_edits_count_as_churn_but_not_as_fixes test runs under it. Do not add a second is_fix && !mass_edit check when merging'
paths: [src/history/mod.rs, src/config.rs]
severity: gotcha
status: active
source: t-1278
fixed_by: null
---
one mass-edit fix gate: [history].max_cochange_commit_size caps both co-change evidence and fix counting; fix_mass_edit_cap (default on) is the knob for the fix half and main's mass_edits_count_as_churn_but_not_as_fixes test runs under it. Do not add a second is_fix && !mass_edit check when merging
