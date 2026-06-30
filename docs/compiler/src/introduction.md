# Introduction

This book explains how the Mirin compiler works — the phases it runs, the
intermediate representations between them, and the query engine that ties them
together. It is written for someone reading or changing the compiler. It is not
a guide to the Mirin language; for that, read the Mirin guide and reference.

Two things to keep in mind as you read.

**The code is the source of truth.** This book orients you and explains *why* the
compiler is shaped the way it is, but where the prose and the code disagree, the
code is right — trust it, and fix the page. Claims tied to a moment carry a date
(*as of 2026-06*), and a design that is intended but not yet built says so.

**The book follows the pipeline.** Mirin compiles a `.mrn` file to SystemVerilog
through a sequence of representations, and the chapters walk that sequence front
to back. The first part stands apart from the order: it covers the architecture
every phase shares — the overview and the query engine. After that, each part
takes one stretch of the pipeline, from source text through to emitted Verilog.

Start with the [Overview](architecture/overview.md).
