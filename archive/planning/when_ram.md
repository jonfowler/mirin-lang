# RAMs: functional update, value-form when, init

Status: landed (the earlier statement-form `when` was reverted — Jon's
call: clocked mutation begs the let-mutation questions (`v[3] = 5;` after
a let) that shouldn't be opened as a RAM side effect).

## The functional RAM

```mirin
var mem: Vec(4, uint(8)) @clk;
mem = init [0x10, 0x20, 0x30, 0x40] when clk.posedge() {
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
- **`init VALUE when …`** — init is an optional PRECEDER on the `when`
  expression (revised from a free-standing statement, per Jon: a
  statement could target a continuously-assigned net, where an SV
  initial block is silently dead — a bug waiting to happen; attached to
  `when`, init-on-a-wire is unrepresentable). POWER-ON state: effective
  in simulation and FPGA bitstreams, ignored by ASIC flows (init is NOT
  reset). The value types as a constant of the produced register's type.
  Orthogonal to and combinable with `.reg` resets.

## Later

- BRAM inference quality: synthesis tools pattern-match
  `mem[waddr] <= data` inside always_ff; the functional form emits a
  whole-array register with a comb copy. The backend can RECOGNISE
  when-tail = replace-of-self-feedback and emit the idiom — semantics
  unchanged, tools happy.
- `init` conflict checking (two inits of one place), file-based init
  ($readmemh-shaped) for big memories.
- `enumerate` became a real method with tuples (planning/tuples.md);
  `replace` is still builtin-typed — a real prelude signature needs
  parametric self types.
