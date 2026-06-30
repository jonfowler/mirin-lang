# Writing Mirin's docs

This guide tells you how to write the documentation under `docs/`. Read it
before you add or migrate a doc. It sets two things: which **register** a doc is
written in (depends on where it lives), and the **prose style** every doc shares
(the same everywhere).

Two reference models stand behind it. For structure, the Rust project's docs —
the [rustc-dev-guide](https://rustc-dev-guide.rust-lang.org/) for compiler
internals, [The Rust Book](https://doc.rust-lang.org/book/) and the
[Reference](https://doc.rust-lang.org/reference/) for the language. For prose,
Williams & Bizup, *Style: Lessons in Clarity and Grace*. When a question isn't
answered here, look at how those four read and imitate them.

> One caveat to state up front, because it shapes everything: **the code is the
> source of truth, not these docs.** A doc explains and orients; it does not
> define behaviour. Say so, pin time-sensitive claims to a date, and point at the
> code (and the relevant `planning/` design note) for the canonical answer. The
> `planning/` tree is design notes and rationale — working memory, not reader
> docs. `docs/` is for readers.

## The two doc trees

| Tree | Audience | Register | Model |
|---|---|---|---|
| `docs/compiler/` | A contributor reading or changing the compiler | Internals guide | rustc-dev-guide |
| `docs/mirin/guide/` | Someone learning to write Mirin | Tutorial | The Rust Book |
| `docs/mirin/reference/` | Someone who knows Mirin and needs the exact rule | Spec | The Rust Reference |

The user docs split into **two** sub-trees with **two voices**. Keep them apart.
Never blend "let's build a counter together" tutorial prose with "the domain of
an aggregate must equal that of its leaves" normative prose in one file. The
guide teaches; the reference adjudicates.

---

## Structure: compiler docs (`docs/compiler/`)

Write these like the rustc-dev-guide: orient the reader, then point them at the
code. Explain *ideas* in prose and *identity* in links — don't paste pass source.

- **Order by the pipeline.** Document stages in the order data flows through them
  (`planning/ir_pipeline.md` is the map: CST → item tree → def map → HIR →
  MIR → SV). Put cross-cutting machinery — the query engine, interning,
  incremental recompute — in an *architecture* section **before** the per-stage
  chapters, so a stage chapter can assume it.
- **Open every stage/IR chapter the same way**, in this order: a one-sentence
  definition; where it sits relative to its neighbours; why it exists; a pointer
  to deeper context. Steal the dev-guide's MIR opener — "MIR is constructed from
  HIR" locates the IR in one clause.
- **Front-load the vocabulary.** For a concept-heavy chapter, list the key terms
  near the top — bold term, one-line definition — rather than defining each
  inline as it first appears.
- **End each section's intro with its point.** The opening paragraph of a section
  closes on a sentence stating what the section establishes and naming the
  concepts that follow. A reader who skims only those sentences should get the
  outline.
- **Keep sections short** (roughly 200–400 words) and split by named noun
  subsections.
- **Link identifiers, cite paths.** Link a type or function name to where it's
  defined; for "where this lives," cite the crate-relative path as inline code
  (`packages/mirin-compiler/src/mir/lower.rs`). Don't reproduce large source
  listings.
- **Show the artifact, not the pass.** Illustrate a stage with the *output it
  produces* — a CST/MIR dump from `--emit cst` / `--emit mir` — and give the
  command to regenerate it, so the example stays reproducible. Don't paste the
  lowering code.
- **Be honest about staleness.** Flag designs that are idealized-not-yet-built,
  date claims that will age ("as of 2026-06"), and name the code as the source
  of truth.

## Structure: the guide (`docs/mirin/guide/`)

Write these like The Rust Book. The structure *is* the teaching method.

- **Motivation before mechanism.** Lead with the problem a feature solves, not
  its grammar. Why before how.
- **Seed intuition before rules.** An analogy or a diagram first; the formal
  rules after.
- **Rules as a short bulleted list,** placed *after* the intuition, never before.
- **Simplest case first, then escalate** to the case that actually needs the
  feature.
- **Teach through failure.** Show code that doesn't compile on purpose, then read
  its error — the diagnostic is part of the lesson.
- **Declare the audience by exclusion.** Say who the guide is *not* for ("assumes
  you've written RTL before") so the right reader self-selects.

## Structure: the reference (`docs/mirin/reference/`)

Write these like the Rust Reference. Precision over warmth; the reader arrived
already oriented.

- **Define by exclusion up front.** Open by stating what the doc is not — not a
  tutorial, assumes familiarity — and link to the guide.
- **State rules normatively.** Use *must* for requirements, *will* for guaranteed
  behaviour, and plain present tense for invariants ("an assignment produces the
  unit value"). Reserve **Note:** blocks for non-normative asides and exceptions.
- **Use a grammar-notation table once,** then write syntax as formal productions.
- **Tabulate matrices; footnote edge semantics.** When the rule space is a grid
  (operators × traits), one row is one rule. Put exact edge behaviour (rounding,
  sign extension, zero-width) in footnotes.
- **Rule first, example second.** The rule leads; an example is supplementary
  proof that the rule is testable, not the explanation itself.
- **Treat spec/implementation disagreement as a bug to file,** not an automatic
  win for either side. This keeps the reference correctable without losing
  authority.

---

## Prose style (every doc)

This is the part that makes a doc clear, and it applies in all three registers.
It comes from Williams & Bizup. The three core principles, in priority order:
**characters as subjects, actions as verbs, old information before new.** If you
have to choose, favour the third.

**Put the actor in the subject and the action in the verb.** A clear sentence
tells a story: who does what. When the action hides inside an abstract noun (a
*nominalization* — `-tion`, `-ment`, `-ance`, `-ing`), the sentence goes limp and
fills with `of`/`by`/`is`.

> ✗ Configuration of the parser occurs at compiler startup.
> ✓ The compiler configures the parser at startup.

> ✗ The resolution of a method's dispatch is a responsibility of `infer`.
> ✓ `infer` resolves a method's dispatch.

Diagnose it by circling the verbs and underlining the subjects, then asking *who
is doing what?* If the real actors and actions aren't in the subjects and verbs,
revise. (Abstractions are fine as actors — `mir_of` *lowers*, the def map
*resolves* — the danger is an abstract noun doing nothing, propped up by an empty
verb like *is*, *has*, *performs*, *conducts*.)

**Old before new — let sentences flow.** Open a sentence with familiar material
(a term from the previous sentence); close it with the new, complex material the
*next* sentence will pick up. The end of one sentence sets up the start of the
next. This is what flow is, and it's why the passive voice is allowed: use it
when it puts the familiar topic first.

**One topic string per passage.** Name the thing a passage is about, then keep
naming it the same way. Don't reach for synonyms "for variety" — repetition of
the topic is what makes a passage cohere. If a section is about MIR, let *MIR* be
the recurring subject.

**State the point where readers look for it** — at the end of a section's
opening, not buried mid-paragraph or saved for the end.

**End on the word that matters.** Readers hear the last few words as stressed.
Get to the verb quickly (avoid long windups and long subjects), and land the
sentence on what you want weighed.

**Be concise — cut what earns nothing.** Run these passes:

- Delete empty words: *actually, really, certain, various, basically, kind of.*
- Delete redundant pairs and implied modifiers: *each and every* → *each*;
  *final outcome* → *outcome*; *period of time* → *period*.
- Replace a phrase with a word: *in the event that* → *if*; *owing to the fact
  that* → *because*; *has the ability to* → *can*; *prior to* → *before*.
- Cut metadiscourse — talk *about* the writing: *"This section introduces the
  problem of …"* → *"One problem is …"*.
- Distrust intensifiers. *Clearly*, *obviously*, and *of course* make a reader
  suspect the opposite. Keep honest hedges (*usually*, *may*, *tends to*) where a
  claim genuinely needs them.

**Shape long sentences with coordination, not pile-up.** Don't chain relative
clause onto relative clause. Coordinate parallel elements, and order them
shorter-to-longer. Worry about length only when sentences run *all* long
(>30 words) or *all* short.

**Drop the folklore.** Split infinitives, sentence-ending prepositions, and
opening with *and*/*but* are not errors — the best writers have done all three
for centuries. Don't contort a sentence to dodge a non-rule.

---

## What not to imitate

`docs/compiler/zero-width-handling.md` is **not** a style model. Its prose packs
too much into each sentence, leans on stacked parentheticals, and drops into
gate-level SystemVerilog before scaffolding the idea. The content is real; the
delivery violates most of the rules above. Don't pattern new docs on it — and
when you migrate it, rewrite it.

## Before you commit a doc

- Right tree, right register? (guide teaches, reference adjudicates, compiler
  orients.)
- Does each section's intro end on its point?
- Did you run the concision passes and hunt nominalizations?
- Examples reproducible (a command to regenerate any dump)?
- Code named as the source of truth; time-sensitive claims dated?

---

## References

- Williams, J. M. & Bizup, J. *Style: Lessons in Clarity and Grace.* The clarity,
  cohesion, coherence, emphasis, and concision principles above are its lessons.
- [rustc-dev-guide](https://rustc-dev-guide.rust-lang.org/) — model for
  `docs/compiler/`. See the [overview](https://rustc-dev-guide.rust-lang.org/overview.html)
  ("what the compiler does to your code" vs "how it does it") and the
  [MIR chapter](https://rustc-dev-guide.rust-lang.org/mir/index.html).
- [The Rust Book](https://doc.rust-lang.org/book/) — model for
  `docs/mirin/guide/`. See [What Is Ownership?](https://doc.rust-lang.org/book/ch04-01-what-is-ownership.html)
  for progressive disclosure.
- [The Rust Reference](https://doc.rust-lang.org/reference/) — model for
  `docs/mirin/reference/`. See [notation](https://doc.rust-lang.org/reference/notation.html).
