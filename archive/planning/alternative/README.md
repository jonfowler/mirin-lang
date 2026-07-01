# Alternative designs

Fully-worked-out alternative approaches to designs in `planning/`. The
**planning-reviewer** (see `.claude/review-agents/`) writes a file here whenever it
proposes a genuinely different approach to a doc under review; the
**implementation-reviewer** reads them back to ask, in hindsight, whether the chosen
plan was the right one.

One file per distinct alternative: `<topic>-<short-slug>.md`. Each should cover the
core idea, how it lowers through the pipeline, what it makes easy, what it makes hard,
and an honest head-to-head against the original. Recording a rejected alternative (with
the reason) is valuable — it stops the option being re-litigated later.
