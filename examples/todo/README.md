# todo-examples

Examples that document intended syntax but are not yet supported by the first-pass compiler. They will move back to `examples/` as the corresponding features land.

- `impl_examples.plr` — depends on static methods / path expressions (`Packet::idle()`) and generic `impl` blocks over a port's domain parameter (exact impl-generics syntax TBD). Plain `impl` methods with their own `{dom …}` already work — see `working/delay_impl.plr`.

Landed and removed from here: `simple_port.plr` and `parameterized_port.plr` (now `working/simple_port.plr`, `working/parameterized_port.plr`), `parameterized_struct.plr` (now `working/parametric_struct.plr` and `working/parametric_struct_extended.plr`), and the `multi_module.plr` stub (its port-impl sketch is covered by `impl_examples.plr`).
