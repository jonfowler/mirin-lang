# Statement-form `when` and RAMs

Status: statement-form landed; initial state is a PROPOSAL (decision open).

## Two whens

- **Value form** (existing): `count = when clk.posedge() { d };` — the
  body's TAIL is the D-input of a synthetic register; the expression's
  value is the held output.
- **Statement form** (new): a bare `when clk.posedge() { … }` whose body
  EQUATIONS are clocked (nonblocking) assignments in one `always_ff`.
  Anything not written this edge HOLDS. `let`s in the body lower
  combinationally (D-input plumbing).

The statement form is the RAM shape: a dynamically indexed element
assignment is the write port, and the decoder is the index mux —

```polar
var mem: Vec(4, uint(8)) @clk;
when clk.posedge() {
    mem[waddr] = if we { wdata } else { mem[waddr] };
}
let rdata = mem[raddr];          // async read; .reg it for sync read
```

emits the textbook inferred-BRAM idiom (`mem[waddr] <= …;` in always_ff).
Driver accounting: a clocked dynamically-indexed write counts as ONE
whole-place drive (hold is the default, so no coverage demand); a second
driver of the same place still conflicts. Combinational dynamic-index
drives remain rejected.

## Initial state (PROPOSAL)

Polar has avoided initial values: state comes from reset (`.reg(rstn,
init)`). RAMs break the pattern — resetting a memory is expensive or
unsynthesizable, while POWER-ON state (FPGA bitstream init; simulation
init) is standard and is what testing wants.

Proposed surface: an `init` clause on `var`:

```polar
var mem: Vec(4, uint(8)) @clk init [0; 4];
var mem2: Vec(4, uint(8)) @clk init [1, 2, 3, 4];
```

- Lowers to an SV `initial` block (`initial mem = '{…};`) — effective in
  simulation and FPGA bitstreams; an ASIC flow ignores it (documented
  caveat: init is not reset).
- The value is a const expression (the literal forms suffice; file-based
  init à la $readmemh is a later extension for big memories).
- Orthogonal to and combinable with reset: `.reg` handles reset; `init`
  handles power-on. Any registered var may take it, not just RAMs.
- Alternatives considered: `var v = e;` already means a driving
  EQUATION, so the initializer position is taken — hence a keyword.

Open for Jon: the `init` keyword + clause shape, and whether
non-vector registered vars should take it from day one.
