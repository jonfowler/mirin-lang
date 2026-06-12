# RAMs: functional update, value-form when, init

Status: landed (the earlier statement-form `when` was reverted — Jon's
call: clocked mutation begs the let-mutation questions (`v[3] = 5;` after
a let) that shouldn't be opened as a RAM side effect).

## The functional RAM

```polar
var mem: Vec(4, uint(8)) @clk;
init mem = [0x10, 0x20, 0x30, 0x40];
mem = when clk.posedge() {
    if we { mem.replace(waddr, wdata) } else { mem }
};
mem[raddr]
```

- **`replace(i, x)`** — a builtin Vec method: a COPY with element i
  swapped (`__repl = v; __repl[i] = x;` in always_comb). Purely
  functional; dynamic index allowed (it is the write decoder); the
  bounds assert applies as for reads.
- **`mem = when E { tail };` registers the LOCAL directly** — always_ff
  on mem's own leaves, no synthetic register, no continuous assign.
  (Also why init works: an initial block on a continuously-assigned net
  is dead.)
- **`init place = value;`** — a separate statement (Verilog's `initial`
  shape, per Jon): POWER-ON state, effective in simulation and FPGA
  bitstreams, ignored by ASIC flows (documented: init is NOT reset).
  Not a drive (doesn't count toward single-assignment/completeness);
  orthogonal to and combinable with `.reg` resets.

## Later

- BRAM inference quality: synthesis tools pattern-match
  `mem[waddr] <= data` inside always_ff; the functional form emits a
  whole-array register with a comb copy. The backend can RECOGNISE
  when-tail = replace-of-self-feedback and emit the idiom — semantics
  unchanged, tools happy.
- `init` conflict checking (two inits of one place), file-based init
  ($readmemh-shaped) for big memories.
- enumerate/replace become real methods when tuples land.
