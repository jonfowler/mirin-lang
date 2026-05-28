# todo-examples

Examples that document intended syntax but are not yet supported by the first-pass compiler. They will move back to `examples/` as the corresponding features land.

- `impl_examples.plr` — depends on lowering of `impl` blocks, path expressions (`Type::method()`), and struct field access.
- `parameterized_port.plr` — depends on parametric type application (`Port{clk}(uint(8))`), which was removed from the first-pass grammar in `e9a247e`.
- `parameterized_struct.plr` — depends on parametric struct types (`struct Bus(A: Type)`) and the same parametric type application at use sites.
