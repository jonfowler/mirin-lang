# todo-examples

Examples that document intended syntax but are not yet supported by the first-pass compiler. They will move back to `examples/` as the corresponding features land.

- `impl_examples.mrn` — depends on static methods / path expressions (`Packet::idle()`) and generic `impl` blocks over a port's domain parameter (exact impl-generics syntax TBD). Plain `impl` methods with their own `{dom …}` already work — see `working/delay_impl.mrn`.
- `mixed-domain-vec-return.mrn` — a valid mixed-domain `Vec`/tuple result wrongly rejected by the top-level-only `@domain` annotation check; compiles once the check derives annotation structurally (planning/aggregate_domains.md Stage 2). Moves to `working/` then.

Landed and removed from here: `simple_port.mrn` and `parameterized_port.mrn` (now `working/simple_port.mrn`, `working/parameterized_port.mrn`), `parameterized_struct.mrn` (now `working/parametric_struct.mrn` and `working/parametric_struct_extended.mrn`), and the `multi_module.mrn` stub (its port-impl sketch is covered by `impl_examples.mrn`).
