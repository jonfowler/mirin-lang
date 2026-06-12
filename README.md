# Mirin

Mirin is an experimental hardware description language focused on RTL
correctness, readability, and high-quality generated SystemVerilog.

> **Status: early and unstable.** The language, compiler, and tooling are under
> active development; syntax and semantics change without notice. Nothing here
> is ready for production use.

## A taste

```mirin
fn multAdd
    {dom clk: Clock, rstn: Reset @clk = high, c: uint(8) @clk = 0}
    (a: uint(8) @clk, b: uint(8) @clk)
    -> uint(8) @clk {
    let mult = a * b;
    let mult = mult.reg(rstn, 0);
    let add = mult + c;
    return add;
}
```

compiles to readable SystemVerilog:

```systemverilog
module multAdd (
    input  logic clk,
    input  logic rstn,
    input  logic [7:0] c,
    input  logic [7:0] a,
    input  logic [7:0] b,
    output logic [7:0] result
);
    logic [7:0] mult;
    assign mult = (a * b);
    logic [7:0] mult_1;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            mult_1 <= 0;
        end else begin
            mult_1 <= mult;
        end
    end
    logic [7:0] add;
    assign add = (mult_1 + c);
    assign result = add;
endmodule
```

Some of the ideas Mirin is built around:

- **Clock domains are types.** A clocked value is written `uint(8) @clk`;
  crossing domains without saying so is a type error. Clocks and resets are
  ordinary (inferable) parameters, not global magic.
- **Ports are first-class.** Module boundaries are described by port types
  with per-field direction, distinct from plain structs.
- **Pipeline-friendly scoping.** `let` is a forward-only sequential binding
  that supports shadowing (`let mult = mult.reg(...)` above); `var` declares a
  signal node that can participate in cyclic equations such as register
  feedback.
- **Readable output.** Generated Verilog keeps source names, deterministic
  naming, and a recoverable hierarchy (e.g. `for` loops become named
  `generate` blocks) so downstream tools and humans can follow it.

More examples live in [`examples/working/`](examples/working/), with their
generated output in `sv/` after a build. Design notes and language decisions
are documented in [`planning/`](planning/).

## Repository layout

| Path | Contents |
| --- | --- |
| `packages/mirin-compiler/` | The compiler: a query-based (salsa) front-to-back implementation emitting SystemVerilog |
| `packages/mirin-lsp/` | Language server built on the compiler's query stack |
| `packages/mirin-fmt/` | Source formatter |
| `packages/tree-sitter-mirin/` | Tree-sitter grammar (concrete syntax, highlighting) |
| `editors/vscode/` | VS Code extension (syntax + LSP client) |
| `planning/` | Design documents — the source of truth for language decisions |
| `examples/`, `fail-examples/` | `.mrn` sources used by the test suite |
| `tests/rtl/` | Behavioural RTL tests (cocotb + verilator) |

## Building and testing

Requires a recent stable Rust toolchain.

```bash
cargo test -p mirin-compiler                            # compiler test suite
cargo run -p mirin-compiler -- examples/working/mult_add.mrn   # compile a .mrn → ./sv/<stem>.sv

tests/rtl/run.sh        # RTL behavioural tests (needs verilator; bootstraps a python venv)

cd packages/tree-sitter-mirin && tree-sitter test       # grammar corpus tests
```

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
